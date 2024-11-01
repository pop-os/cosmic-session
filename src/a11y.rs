use futures_util::StreamExt;
use launch_pad::ProcessManager;
use tokio::sync::mpsc;
use tracing::Instrument;

const ORCA: Option<&'static str> = option_env!("ORCA");

pub async fn start_a11y(
	env_vars: Vec<(String, String)>,
	pman: ProcessManager,
) -> color_eyre::Result<()> {
	let (tx, mut rx) = mpsc::unbounded_channel();
	let mut process_key = None;
	let conn = zbus::Connection::session().await?;
	let proxy = cosmic_dbus_a11y::StatusProxy::new(&conn).await?;

	tokio::spawn(async move {
		let mut watch_changes = proxy.receive_screen_reader_enabled_changed().await;
		let mut enabled = false;
		if let Ok(status) = proxy.screen_reader_enabled().await {
			_ = tx.send(status);

			enabled = status;
		}
		while let Some(change) = watch_changes.next().await {
			let Ok(new_enabled) = change.get().await else {
				tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
				continue;
			};
			if enabled != new_enabled {
				_ = tx.send(new_enabled);
				enabled = new_enabled;
			}
		}
	});

	while let Some(enabled) = rx.recv().await {
		let stdout_span = info_span!(parent: None, "screen-reader");
		let stderr_span = stdout_span.clone();
		if enabled && process_key.is_none() {
			// spawn orca
			match pman
				.start(
					launch_pad::process::Process::new()
						.with_executable(ORCA.unwrap_or("/usr/bin/orca"))
						.with_env(env_vars.clone())
						.with_on_stdout(move |_, _, line| {
							let stdout_span = stdout_span.clone();
							async move {
								info!("{}", line);
							}
							.instrument(stdout_span)
						})
						.with_on_stderr(move |_, _, line| {
							let stderr_span = stderr_span.clone();
							async move {
								warn!("{}", line);
							}
							.instrument(stderr_span)
						}),
				)
				.await
			{
				Ok(key) => {
					process_key = Some(key);
				}
				Err(err) => {
					tracing::error!("Failed to start screen reader {err:?}");
				}
			}
		} else if !enabled && process_key.is_some() {
			// kill orca
			info!("Stopping screen reader");
			if let Err(err) = pman.stop_process(process_key.take().unwrap()).await {
				tracing::error!("Failed to stop screen reader. {err:?}")
			}
		}
	}
	Ok(())
}
