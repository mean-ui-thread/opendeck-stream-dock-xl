use futures_lite::StreamExt;
use mirajazz::{
    device::{DeviceWatcher, list_devices},
    error::MirajazzError,
    types::{DeviceLifecycleEvent, HidDeviceInfo},
};
use openaction::device_plugin;
use tokio::time::{self, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use crate::{
    DEVICES, TOKENS, TRACKER,
    device::device_task,
    mappings::{CandidateDevice, DEVICE_NAMESPACE, Kind, QUERIES},
};

fn serial_to_id(serial: &String) -> String {
    format!("{}-{}", DEVICE_NAMESPACE, serial)
}

fn device_info_to_candidate(dev: HidDeviceInfo) -> Option<CandidateDevice> {
        let id = serial_to_id(&dev.serial_number.clone()?);
    let kind = Kind::from_vid_pid(dev.vendor_id, dev.product_id)?;

    Some(CandidateDevice { id, dev, kind })
}

/// Returns devices that matches known pid/vid pairs
async fn get_candidates() -> Result<Vec<CandidateDevice>, MirajazzError> {
    log::info!("Looking for candidate devices");

    let mut candidates: Vec<CandidateDevice> = Vec::new();

    for dev in list_devices(&QUERIES).await? {
        if let Some(candidate) = device_info_to_candidate(dev.clone()) {
            candidates.push(candidate);
        } else {
            continue;
        }
    }

    Ok(candidates)
}

pub async fn watcher_task(token: CancellationToken) -> Result<(), MirajazzError> {
    log::info!("Watcher task starting");

    let tracker = TRACKER.lock().await.clone();

    // Scans for connected devices that (possibly) we can use
    log::info!("Looking for connected devices");
    let candidates = get_candidates().await?;

    log::debug!("Found {} candidate devices during initial scan", candidates.len());

    for candidate in candidates {
        log::info!("Initial candidate found: {}", candidate.id);

        let token = CancellationToken::new();

        TOKENS
            .write()
            .await
            .insert(candidate.id.clone(), token.clone());

        tracker.spawn(device_task(candidate, token));
    }

    let mut watcher = DeviceWatcher::new();
    let mut watcher_stream = watcher.watch(&QUERIES).await?;
    let mut rescan_interval = time::interval(std::time::Duration::from_secs(5));
    rescan_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

    log::info!("Watcher is ready");

    loop {
        let ev = tokio::select! {
            v = watcher_stream.next() => v,
            _ = rescan_interval.tick() => {
                let candidates = match get_candidates().await {
                    Ok(candidates) => candidates,
                    Err(err) => {
                        log::error!("Periodic candidate rescan failed: {}", err);
                        continue;
                    }
                };

                for candidate in candidates {
                    if DEVICES.read().await.contains_key(&candidate.id) {
                        continue;
                    }

                    if TOKENS.read().await.contains_key(&candidate.id) {
                        continue;
                    }

                    log::info!("Rescan found missing tracked device {}; spawning task", candidate.id);

                    let token = CancellationToken::new();

                    TOKENS
                        .write()
                        .await
                        .insert(candidate.id.clone(), token.clone());

                    tracker.spawn(device_task(candidate, token));
                }

                continue;
            },
            _ = token.cancelled() => None
        };

        if let Some(ev) = ev {
            log::info!("New device event: {:?}", ev);

            match ev {
                DeviceLifecycleEvent::Connected(info) => {
                    if let Some(candidate) = device_info_to_candidate(info) {
                        // Don't add existing device again
                        if DEVICES.read().await.contains_key(&candidate.id) {
                            log::info!("Ignoring duplicate connect event for already tracked device {}", candidate.id);
                            continue;
                        }

                        log::info!("Device connected: {}", candidate.id);

                        let token = CancellationToken::new();

                        TOKENS
                            .write()
                            .await
                            .insert(candidate.id.clone(), token.clone());

                        log::info!("Spawning task for new device: {}", candidate.id);
                        tracker.spawn(device_task(candidate, token));
                        log::info!("Spawned device task for newly connected device");
                    }
                }
                DeviceLifecycleEvent::Disconnected(info) => {
                    let id = serial_to_id(&info.serial_number.unwrap());

                    if let Some(token) = TOKENS.write().await.remove(&id) {
                        log::info!("Sending cancel request for {}", id);
                        token.cancel();
                    }

                    DEVICES.write().await.remove(&id);

                    device_plugin::unregister_device(id.clone()).await.ok();

                    log::info!("Disconnected device {}", id);
                }
            }
        } else {
            log::info!("Watcher is shutting down");

            break Ok(());
        }
    }
}
