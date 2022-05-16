# ECS Service Metrics Exporter

## Motivation
Prometheus has auto discovery for EC2 instances, but this doesn't include scraping any services that run on an ECS cluster. After not finding a good
concept, this exporter was born. One benefit/drawback is that every service can use a different port for exposing the metrics and this port does **not** have
to be exposed by Docker. This works well if, for example, you run Spring Actuator on a different port than the main traffic port, so the metrics will not be
accidentially exposed on a ELB.

## What it does
This exporter is meant for AWS ECS (Elastic Container Service) environments, where services running in ECS expose metrics in the OpenMetrics text format.
It scrapes metrics from all running services that support it and makes the combined scraped metrics of all services available via a single HTTP endpoint.

When deployed within an ECS cluster, the service should run in `daemon` mode (so it is deployed to each instance in the cluster) as well as given
permission to access the Docker daemon. For this, the easiest way is to mount the Docker socket into the container, but please be **careful** about
security implications (see below). Allowing access to the Docker socket will effectively grant this app root privileges on the EC2 instance it is running on.

When the app's HTTP endpoint `/metrics` is called, it will connect to the Docker daemon and enumerate all container that have a specific label. The label
name can be configured via the environment (see `.env.example` for configuration). The label for each service should contain the service's port and path,
like so: `9100/path/to/metrics`. This allows flexible configuration for each service, on which port and under which path the metrics are available.
For all running containers, the app will then try to execute a cURL command `curl -s http://localhost:$PORT$$METRICPATH$`. For the above example, it would
execute the command `curl -s http://localhost:9100/path/to/metrics`.

The output of the metrics of all services will be combined and returned to the (HTTP) caller. To allow discriminiating metrics by service, and extra label
`container_name` is added to every metric. The source of this label is the Docker label `com.amazonaws.ecs.container-name`.

## Howto
1. Build this service as a Docker image and put it into your Docker registry (most likely AWS ECR)
2. Create a task definition and a service in ECS. Here's an example in CDK with TypeScript
```typescript
const taskDefinition = new Ec2TaskDefinition(this, 'task-definition', {
	networkMode: NetworkMode.BRIDGE,
});

taskDefinition.addVolume({
	host: {
		sourcePath: '/var/run/docker.sock'
	},
	name: 'docker-socket',
});

const container = taskDefinition.addContainer('task-definition', {
	image: ContainerImage.fromEcrRepository(yourEcrRepositoryWithThisImage),
	memoryLimitMiB: 128,
	environment: {
		HTTP_LISTEN_PORT: '9102',
		RUST_LOG: 'info,ecs_service_metrics_exporter=debug',
		DOCKER_LABEL_HAS_METRICS: 'container-exposes-metrics',
	},
	portMappings: [
		{
			containerPort: '9102',
			hostPort: '9102',
			protocol: EcsProtocol.TCP
		}
	],
});

container.addMountPoints({containerPath: '/var/run/docker.sock', readOnly: true, sourceVolume: 'docker-socket'});

const service = new Ec2Service(this, 'ecs-service', {
	cluster,
	// this is important, to that one instance is scheduled on every ECS instance in your cluster
	daemon: true,
	taskDefinition,
});
```
3. Attach a label (in above example: `container-exposes-metrics`) to all containers (in the task definition's container) that contain the port and path
   at which metrics are exposed, e.g. `8080/metrics` or `9100/path/to/metrics`
4. Make sure all of these container you attach the label to have cURL installed
5. Now you should be able to instruct Prometheus to use EC2 discovery for your ECS instances and fetch the combined metrics

## Security

Please be very aware of the following points:
* This app effectively has root access in the EC2 instance **!!!** since it needs full access to the Docker daemon
* The integrated HTTP endpoint has no encryption, so take that into account
* Be sure to secure your HTTP endpoint with a security group (or firewall)
