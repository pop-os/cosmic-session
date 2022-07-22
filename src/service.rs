// SPDX-License-Identifier: GPL-3.0-only
use tokio::sync::oneshot;
use zbus::dbus_interface;

pub struct SessionService {
	pub exit_tx: Option<oneshot::Sender<()>>,
}

#[dbus_interface(name = "com.system76.CosmicSession")]
impl SessionService {
	async fn exit(&mut self) {
		match self.exit_tx.take() {
			Some(tx) => {
				tx.send(()).ok();
			}
			None => {
				warn!("previously failed to properly exit session");
			}
		}
	}
}
