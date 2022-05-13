use std::{env, process};
use std::collections::HashMap;

use bollard::container::{ListContainersOptions, LogOutput};
use bollard::Docker;
use bollard::errors::Error;
use bollard::exec::{CreateExecOptions, CreateExecResults, StartExecResults};
use bollard::models::{ContainerSummary};
use dotenv::dotenv;
use futures_util::TryStreamExt;
use log::{debug, error, info, warn};

// TODO Change back to 9100
const DEFAULT_SERVICE_PORT_PATH: &str = "8080/metrics";
const UNKNOWN_SERVICE_NAME: &str = "unknown-service";

const CONFIG_KEY_DOCKER_LABEL_HAS_METRICS: &'static str = "DOCKER_LABEL_HAS_METRICS";

#[tokio::main]
async fn main() {
	dotenv().ok();
	env_logger::init();
	let docker = Docker::connect_with_socket_defaults().unwrap();
	let docker_label_has_metrics = get_env_variable_as_str(CONFIG_KEY_DOCKER_LABEL_HAS_METRICS);
	let containers = get_docker_containers_matching_label(&docker, &docker_label_has_metrics).await;

	if let Err(err) = containers {
		warn!("Failed to get list of Docker containers, e={:?}", err);
		return;
	}

	let containers = containers.unwrap();
	debug!("Found {} running containers matching the required label", containers.len());
	let mut metrics = String::new();

	for container in containers {
		let container_id = &container.id.clone().unwrap();
		let aws_container_name = &container.labels.clone()
			.unwrap_or(HashMap::new())
			.get("com.amazonaws.ecs.container-name")
			.unwrap_or(&UNKNOWN_SERVICE_NAME.to_string())
			.to_string();
		let curl_exec = create_docker_exec_for_curl(&docker, &docker_label_has_metrics, container, &container_id).await;

		if let Err(err) = curl_exec {
			warn!("Failed to create exec in container={:?}, e={:?}", &container_id, err);
			continue;
		}

		let exec_id = curl_exec.unwrap().id;
		let curl_output = start_curl_exec_return_logs(&docker, container_id, &exec_id).await;
		let exit_code: i64= match docker.inspect_exec(&exec_id).await {
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
			.map(|line| add_service_name_to_metric_line(line, aws_container_name))
			.collect::<Vec<String>>()
			.join("\n").as_str();
	}

	debug!("Found metrics={}", metrics);
}

fn get_env_variable_as_str(key: &str) -> String {
	env::var(key).map_err(move |_| {
		error!("Missing mandatory config key={}, exiting", key);
		process::exit(1);
	})
		.unwrap()
}

async fn get_docker_containers_matching_label(docker: &Docker, docker_label_has_metrics: &String) -> Result<Vec<ContainerSummary>, Error> {
	let mut container_filters = HashMap::new();
	container_filters.insert("label", vec![docker_label_has_metrics.as_str()]);

	// get a list of Docker containers
	let containers = docker.list_containers(Some(ListContainersOptions {
		all: false,
		limit: None,
		size: false,
		filters: container_filters,
	}))
		.await;
	containers
}

async fn create_docker_exec_for_curl(docker: &Docker, docker_label_has_metrics: &String, container: ContainerSummary, container_id: &String) -> Result<CreateExecResults, Error> {
	let port_and_metric_path = container.labels.unwrap_or(HashMap::new())
		.get(docker_label_has_metrics)
		.unwrap_or(&DEFAULT_SERVICE_PORT_PATH.to_string())
		.to_string();

	let curl_url = format!("http://localhost:{}", port_and_metric_path);
	let curl_command = vec!["/bin/curl", "-s", curl_url.as_str()];

	docker.create_exec(&container_id, CreateExecOptions {
			attach_stdout: Some(true),
			attach_stderr: Some(false),
			cmd: Some(curl_command),
			..Default::default()
		})
		.await
}

async fn start_curl_exec_return_logs(docker: &Docker, container_id: &String, exec_id: &String) -> Option<Vec<String>> {
	match docker.start_exec(&exec_id, None).await {
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

fn add_service_name_to_metric_line(line: &String, container_name: &str) -> String {
	// return comment/meta lines unaltered
	if line.trim().starts_with("#") {
		return line.to_string();
	}

	let service_label = format!("container_name={}", container_name);

	// already has a label => add our label as the first one, including a trailing comma
	if let Some(bracket_position) = line.find("{") {
		let (line_left, line_right) = line.split_at(bracket_position + 1);
		return format!("{}{},{}", line_left, service_label, line_right).to_string();
	}

	// no label yet => insert the whole label thingy
	if let Some(space_pos) = line.find(" ") {
		let (line_left, line_right) = line.split_at(space_pos);
		return format!("{}{{{}}}{}", line_left, service_label, line_right).to_string();
	}

	info!("Encountered a weird line, neither comment nor parsable metric, not attaching service name: {}", line);
	line.to_string()
}
