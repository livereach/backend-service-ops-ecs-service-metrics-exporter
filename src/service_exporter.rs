use std::collections::HashMap;

use bollard::container::{ListContainersOptions, LogOutput};
use bollard::Docker;
use bollard::errors::Error as BollardError;
use bollard::exec::{CreateExecOptions, CreateExecResults, StartExecResults};
use bollard::models::ContainerSummary;
use futures_util::stream::StreamExt;
use log::{debug, info, warn};
use tokio::task::JoinHandle;

const DEFAULT_SERVICE_PORT_PATH: &str = "9100/metrics";
const UNKNOWN_SERVICE_NAME: &str = "unknown-service";

pub struct ServiceMetricsExporter {
	docker: Docker,
	label_has_metrics: String,
}

impl Clone for ServiceMetricsExporter {
	fn clone(&self) -> ServiceMetricsExporter {
		ServiceMetricsExporter {
			docker: self.docker.clone(),
			label_has_metrics: self.label_has_metrics.clone(),
		}
	}
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
		let mut join_handles: HashMap<String, JoinHandle<Option<String>>> = HashMap::new();
		let mut all_metrics = String::new();

		for container in containers {
			let _self = self.clone();
			let container_id = &container.id.clone().unwrap();

			let join_handle = tokio::spawn(async move {
				let container_id = &container.id.clone().unwrap();
				let aws_container_name = &container.labels.clone()
					.unwrap_or_default()
					.get("com.amazonaws.ecs.container-name")
					.unwrap_or(&UNKNOWN_SERVICE_NAME.to_string())
					.to_string();
				let curl_exec = _self.create_docker_exec_for_curl(container, container_id).await;

				if let Err(err) = curl_exec {
					warn!("[Container {}]: Failed to create exec, e={:?}", &container_id, err);
					return None;
				}

				let exec_id = curl_exec.unwrap().id;
				let curl_output = _self.start_curl_exec_return_logs(container_id, &exec_id).await;
				let exit_code: i64 = match _self.docker.inspect_exec(&exec_id).await {
					Ok(res) => res.exit_code.unwrap_or(-1),
					Err(err) => {
						warn!("[Container {}]: Failed to get exit code for exec_id={}, e={:?}", &container_id, &exec_id, err);
						-1
					}
				};

				if exit_code != 0 || curl_output.is_none() {
					warn!("[Container {}]: Exit code for exec={} is {}, output={:?}", &container_id, &exec_id, exit_code, curl_output);
					return None;
				}

				let result = curl_output.unwrap().iter()
					.map(|line| _self.add_service_name_to_metric_line(container_id, aws_container_name, line))
					.collect::<Vec<String>>()
					.join("\n").as_str()
					.to_string();
				Some(result)
			});

			join_handles.insert(container_id.to_string(), join_handle);
		}

		for join_handle in join_handles {
			match join_handle.1.await {
				Ok(Some(container_metrics)) => all_metrics.push_str(container_metrics.as_str()),
				Ok(None) => debug!("[Container {}]: Returned no metrics", join_handle.0),
				_ => warn!("[Container {}]: Failed to collect metrics from container", join_handle.0),
			};
		}

		Some(all_metrics)
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
		let mut output = match self.docker.start_exec(exec_id, None).await {
			Ok(StartExecResults::Attached { output, .. }) => output,
			Ok(StartExecResults::Detached) => {
				warn!("[Container {}]: Somehow got unexpected detached cURL", container_id);
				return None;
			}
			Err(err) => {
				warn!("[Container {}]: Failed to start cURL exec, e={:?}", container_id, err);
				return None;
			}
		};

		debug!("[Container {}]: Got result from cURL", container_id);
		let mut result: Vec<u8> = vec![];

		while let Some(out_data) = output.next().await {
			match out_data {
				Ok(LogOutput::StdOut { message }) => {
					result.append(&mut message.to_vec());
				}
				Ok(LogOutput::StdErr { message }) => info!("[Container {}]: Got stderr={:?}", container_id, String::from_utf8_lossy(&message.to_vec())),
				Err(err) => { debug!("Got ERR line={:?}", err) }
				_ => {}
			};
		}

		Some(String::from_utf8_lossy(&result).split('\n').map(|s| s.to_string()).collect())
	}

	fn add_service_name_to_metric_line(&self, container_id: &String, container_name: &str, line: &String) -> String {
		// return comment/meta lines unaltered
		if line.trim().starts_with('#') {
			return line.to_string();
		}

		// ignore any empty lines
		if line.is_empty() {
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

		info!("[Container {}]: Encountered a weird line, neither comment nor parsable metric, not attaching service name: {}", container_id, line);
		line.to_string()
	}
}
