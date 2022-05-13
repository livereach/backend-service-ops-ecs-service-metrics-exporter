use std::{env, process};
use std::future::Future;
use std::sync::Arc;

use dotenv::dotenv;
use log::{debug, error};
use tokio::signal;
use tokio::sync::oneshot;
use tokio::sync::oneshot::Receiver;
use warp::Filter;

use crate::service_exporter::ServiceMetricsExporter;

mod service_exporter;

const CONFIG_KEY_DOCKER_LABEL_HAS_METRICS: &str = "DOCKER_LABEL_HAS_METRICS";
const CONFIG_KEY_HTTP_LISTEN_PORT: &str = "HTTP_LISTEN_PORT";

#[tokio::main]
async fn main() {
	dotenv().ok();
	env_logger::init();

	let (term_tx, term_rx) = oneshot::channel();
	let service_exporter = ServiceMetricsExporter::new(get_env_variable_as_str(CONFIG_KEY_DOCKER_LABEL_HAS_METRICS));
	let server = create_warp_server(term_rx, service_exporter).await;
	tokio::task::spawn(server);

	match signal::ctrl_c().await {
		Ok(()) => {
			term_tx.send(()).ok();
		}
		Err(err) => {
			error!("Unable to listen for shutdown signal: {}", err);
			process::exit(1);
		}
	}

	debug!("Received signal, exiting main method");
}

fn get_env_variable_as_str(key: &str) -> String {
	env::var(key).map_err(move |_| {
		error!("Missing mandatory config key={}, exiting", key);
		process::exit(1);
	})
		.unwrap()
}

fn get_env_variable_as_int(key: &str, default_value: u16) -> u16 {
	let as_str = env::var(key).map_err(move |_| {
		error!("Missing mandatory config key={}, exiting", key);
		process::exit(1);
	})
		.unwrap();

	match as_str.parse::<u16>() {
		Ok(n) => n,
		Err(_) => default_value
	}
}

async fn create_warp_server(term_rx: Receiver<()>, service_exporter: ServiceMetricsExporter) -> impl Future<Output=()> {
	let health_route = warp::get()
		.and(warp::path("health").map(|| "OK"));

	let service_exporter = Arc::new(service_exporter);
	let metric_route = warp::get()
		.and(warp::path("metrics"))
		.and(warp::any().map(move || service_exporter.clone()))
		.and_then(|service_exporter: Arc<ServiceMetricsExporter>| async move {
			service_exporter.export_metrics().await
		});

	let routes = health_route.or(metric_route);
	let listen_port = get_env_variable_as_int(CONFIG_KEY_HTTP_LISTEN_PORT, 8080);
	let (_, server) = warp::serve(routes)
		.bind_with_graceful_shutdown(([0, 0, 0, 0], listen_port), async {
			term_rx.await.ok();
		});
	debug!("Started HTTP server on port={}", listen_port);
	server
}
