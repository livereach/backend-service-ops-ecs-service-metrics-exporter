use std::collections::HashMap;

use bollard::container::{ListContainersOptions, LogOutput};
use bollard::Docker;
use bollard::errors::Error as BollardError;
use bollard::exec::{CreateExecOptions, CreateExecResults, StartExecResults};
use bollard::models::ContainerSummary;
use futures_util::TryStreamExt;
use log::{debug, info, warn};

const DEFAULT_SERVICE_PORT_PATH: &str = "9100/metrics";
const UNKNOWN_SERVICE_NAME: &str = "unknown-service";

pub struct ServiceMetricsExporter {
	docker: Docker,
	label_has_metrics: String,
}


impl ServiceMetricsExporter {
	pub fn new(label_has_metrics: String) -> ServiceMetricsExporter {
		ServiceMetricsExporter {
			docker: Docker::connect_with_socket_defaults().unwrap(),
			label_has_metrics,
		}
	}

	pub async fn export_metrics(&self) -> Result<String, warp::Rejection> {
		debug!("Handling request for getting metrics");
		let metrics = self.get_combined_metrics_from_all_containers().await;

		match metrics {
			None => Err(warp::reject()),
			Some(metrics) => Ok(metrics),
		}
	}

	async fn get_combined_metrics_from_all_containers(&self) -> Option<String> {
		let containers = self.get_docker_containers_matching_label().await;

		if let Err(err) = containers {
			warn!("Failed to get list of Docker containers, e={:?}", err);
			return None;
		}

		let containers = containers.unwrap();
		debug!("Found {} running containers matching the required label", containers.len());
		let mut metrics = String::new();

		for container in containers {
			let container_id = &container.id.clone().unwrap();
			let aws_container_name = &container.labels.clone()
				.unwrap_or_default()
				.get("com.amazonaws.ecs.container-name")
				.unwrap_or(&UNKNOWN_SERVICE_NAME.to_string())
				.to_string();
			let curl_exec = self.create_docker_exec_for_curl(container, container_id).await;

			if let Err(err) = curl_exec {
				warn!("Failed to create exec in container={:?}, e={:?}", &container_id, err);
				continue;
			}

			let exec_id = curl_exec.unwrap().id;
			let curl_output = self.start_curl_exec_return_logs(container_id, &exec_id).await;
			let exit_code: i64 = match self.docker.inspect_exec(&exec_id).await {
				Ok(res) => res.exit_code.unwrap_or(-1),
				Err(err) => {
					warn!("Failed to get exit code for exec_id={}, e={:?}", &exec_id, err);
					-1
				}
			};

			if exit_code != 0 || curl_output.is_none() {
				warn!("Exit code for exec={} in container={} is {}, output={:?}", &exec_id, &container_id, exit_code, curl_output);
				continue;
			}

			metrics += curl_output.unwrap().iter()
				.map(|line| self.add_service_name_to_metric_line(line, aws_container_name))
				.collect::<Vec<String>>()
				.join("\n").as_str();
		}

		Some(metrics)
	}

	async fn get_docker_containers_matching_label(&self) -> Result<Vec<ContainerSummary>, BollardError> {
		let mut container_filters = HashMap::new();
		container_filters.insert("label", vec![self.label_has_metrics.as_str()]);

		self.docker.list_containers(Some(ListContainersOptions {
			all: false,
			limit: None,
			size: false,
			filters: container_filters,
		}))
			.await
	}

	async fn create_docker_exec_for_curl(&self, container: ContainerSummary, container_id: &str) -> Result<CreateExecResults, BollardError> {
		let port_and_metric_path = container.labels.unwrap_or_default()
			.get(&self.label_has_metrics)
			.unwrap_or(&DEFAULT_SERVICE_PORT_PATH.to_string())
			.to_string();

		let curl_url = format!("http://localhost:{}", port_and_metric_path);
		let curl_command = vec!["/bin/curl", "-s", curl_url.as_str()];

		self.docker.create_exec(container_id, CreateExecOptions {
			attach_stdout: Some(true),
			attach_stderr: Some(false),
			cmd: Some(curl_command),
			..Default::default()
		})
			.await
	}

	async fn start_curl_exec_return_logs(&self, container_id: &String, exec_id: &str) -> Option<Vec<String>> {
		match self.docker.start_exec(exec_id, None).await {
			Ok(StartExecResults::Attached { output, .. }) => {
				debug!("Started cURL in container={}", &container_id);
				let log = output.try_collect().await;
				if let Err(err) = log {
					debug!("Failed to get output for container={}, e={:?}", &container_id, err);
					return None;
				}

				let log: Vec<_> = log.unwrap();

				if log.is_empty() {
					warn!("Found no output log for container={}", &container_id);
					return None;
				}

				let mut output_lines = vec![];
				match &log[0] {
					LogOutput::StdOut { message } => {
						for line in String::from_utf8_lossy(message).split('\n') {
							output_lines.push(line.to_string());
						}
					}
					LogOutput::StdErr { .. } => {}
					LogOutput::StdIn { .. } => {}
					LogOutput::Console { .. } => {}
				};
				Some(output_lines)
			}
			Ok(StartExecResults::Detached) => {
				warn!("Somehow failed to start cURL in container={} => detached", &container_id);
				None
			}
			Err(err) => {
				warn!("Failed to start cURL exec in container={}, e={:?}", &container_id, err);
				None
			}
		}
	}

	fn add_service_name_to_metric_line(&self, line: &String, container_name: &str) -> String {
		// return comment/meta lines unaltered
		if line.trim().starts_with('#') {
			return line.to_string();
		}

		let service_label = format!("container_name={}", container_name);

		// already has a label => add our label as the first one, including a trailing comma
		if let Some(bracket_position) = line.find('{') {
			let (line_left, line_right) = line.split_at(bracket_position + 1);
			return format!("{}{},{}", line_left, service_label, line_right);
		}

		// no label yet => insert the whole label thingy
		if let Some(space_pos) = line.find(' ') {
			let (line_left, line_right) = line.split_at(space_pos);
			return format!("{}{{{}}}{}", line_left, service_label, line_right);
		}

		info!("Encountered a weird line, neither comment nor parsable metric, not attaching service name: {}", line);
		line.to_string()
	}

}
