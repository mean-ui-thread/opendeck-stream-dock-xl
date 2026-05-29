use data_url::DataUrl;
use image::{imageops::FilterType, load_from_memory_with_format};
use mirajazz::{device::Device, error::MirajazzError, state::DeviceStateUpdate};
use openaction::{device_plugin, global_events::SetImageEvent};
use std::{sync::Arc, time::{Duration, SystemTime}};
use tokio::time::{self, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use crate::{
    BRIGHTNESS_CACHE, DEVICES, IMAGE_CACHE, TOKENS, led_config,
    inputs::button_key,
    mappings::{
        COL_COUNT, CandidateDevice, DEVICE_TYPE, ENCODER_COUNT, KEY_COUNT, ROW_COUNT,
        image_format,
    },
};

pub const HEART_BEAT_TIME: u64 = 10; // device powers off every 35 seconds seconds without receiving any command, so send keep-alive every 10 seconds just to be safe
const ENCODER_REPEAT_INITIAL_DELAY_SECS: u64 = 1;
const ENCODER_REPEAT_INTERVAL_MILLIS: u64 = 50;

async fn cleanup_device_state(id: &str, cancel_token: bool) {
    log::info!("Cleaning up device state for {}", id);

    if cancel_token {
        if let Some(token) = TOKENS.write().await.remove(id) {
            token.cancel();
        }
    } else {
        TOKENS.write().await.remove(id);
    }

    DEVICES.write().await.remove(id);

    if let Err(err) = device_plugin::unregister_device(id.to_string()).await {
        log::error!("Failed to deregister device {}: {}", id, err);
    }
}

/// Initializes a device and listens for events
pub async fn device_task(candidate: CandidateDevice, token: CancellationToken) {
    log::info!("Running device task for {:?}", candidate);
    log::info!("Starting device initialization for {}", candidate.id);

    // Wrap in a closure so we can use `?` operator
    let device = async || -> Result<Device, MirajazzError> {
        log::info!("Connecting to raw HID device for {}", candidate.id);
        let device = connect(&candidate).await?;

        // Do not clear key images during init. If OpenDeck does not resend SetImage events
        // after reconnect, clearing here permanently loses the user's visible key state.
        log::info!("Applying initial brightness for {}", candidate.id);
        device.set_brightness(50).await?;

        let led = led_config::load();
        log::info!("Applying LED config: {:?}", led);
        if let Some(led_config::LedMode::Static { colors }) = led.mode {
            device.set_led_brightness(led.brightness).await?;
            device.set_led_colors(&colors).await?;
        }

        Ok(device)
    }()
    .await;

    let device: Device = match device {
        Ok(device) => device,
        Err(err) => {
            handle_error(&candidate.id, err).await;

            log::error!(
                "Had error during device init, finishing device task: {:?}",
                candidate
            );

            return;
        }
    };

    let device = Arc::new(device);

    log::info!("Registering device {}", candidate.id);

    if let Err(err) = device_plugin::register_device(
        candidate.id.clone(),
        candidate.kind.human_name(),
        ROW_COUNT as u8,
        COL_COUNT as u8,
        ENCODER_COUNT as u8,
        DEVICE_TYPE,
    )
    .await
    {
        log::error!("Failed to register device {}: {}", candidate.id, err);
    }

    DEVICES
        .write()
        .await
        .insert(candidate.id.clone(), device.clone());

    if let Err(err) = replay_cached_state(&candidate.id, device.as_ref()).await {
        log::error!("Failed to replay cached state for {}: {}", candidate.id, err);
    }

    log::info!("Device {} registered and stored; starting event and heartbeat tasks", candidate.id);

    tokio::select! {
        _ = device_events_task(&candidate, device.clone()) => {},
        _ = keep_alive_task(&candidate, device.clone()) => {},
        _ = token.cancelled() => {}
    };

    log::info!("Shutting down device {:?}", candidate);

    device.shutdown().await.ok();
    cleanup_device_state(&candidate.id, false).await;

    log::info!("Device task finished for {:?}", candidate);
}

async fn replay_cached_state(id: &str, device: &Device) -> Result<(), MirajazzError> {
    let cached_brightness = BRIGHTNESS_CACHE.read().await.get(id).copied();

    if let Some(brightness) = cached_brightness {
        log::info!("Replaying cached brightness {} for {}", brightness, id);
        device.set_brightness(brightness).await?;
    }

    let cached_images = IMAGE_CACHE.read().await.get(id).cloned();

    if let Some(images) = cached_images {
        if images.is_empty() {
            log::info!("No cached button images to replay for {}", id);
            return Ok(());
        }

        log::info!("Replaying {} cached button images for {}", images.len(), id);

        let mut positions = images.keys().copied().collect::<Vec<u8>>();
        positions.sort_unstable();

        for position in positions {
            let image = images.get(&position).cloned().unwrap_or(None);

            let event = SetImageEvent {
                device: id.to_string(),
                controller: None,
                position: Some(position),
                image,
            };

            handle_set_image(device, event).await?;
        }
    } else {
        log::info!("No cached state found for {}; waiting for OpenDeck image events", id);
    }

    Ok(())
}

/// Handles errors, returning true if should continue, returning false if an error is fatal
pub async fn handle_error(id: &String, err: MirajazzError) -> bool {
    log::error!("Device {} error: {}", id, err);

    // Some errors are not critical and can be ignored without sending disconnected event
    if matches!(err, MirajazzError::ImageError(_) | MirajazzError::BadData) {
        return true;
    }

    log::info!("Deregistering device {}", id);
    cleanup_device_state(id, true).await;

    log::info!("Finished clean-up for {}", id);

    false
}

pub async fn connect(candidate: &CandidateDevice) -> Result<Device, MirajazzError> {
    let result = Device::connect(
        &candidate.dev,
        candidate.kind.protocol_version(),
        KEY_COUNT,
        ENCODER_COUNT,
    )
    .await;

    match result {
        Ok(device) => {
            Ok(device
                .with_supports_both_encoder_states(candidate.kind.supports_both_encoder_states()))
        }
        Err(e) => {
            log::error!("Error while connecting to device: {e}");

            Err(e)
        }
    }
}

/// Handles events from device to OpenDeck
async fn device_events_task(
    candidate: &CandidateDevice,
    _device: Arc<Device>,
) -> Result<(), MirajazzError> {
    log::info!("Connecting to {} for incoming events", candidate.id);

    let mut encoder_repeat_tokens: [Option<CancellationToken>; ENCODER_COUNT] =
        std::array::from_fn(|_| None);

    let devices_lock = DEVICES.read().await;
    let reader = match devices_lock.get(&candidate.id) {
        Some(device) => device.get_reader(crate::inputs::process_input),
        None => return Ok(()),
    };
    drop(devices_lock);

    log::info!("Connected to {} for incoming events", candidate.id);

    log::info!("Reader is ready for {}", candidate.id);

    loop {
        log::debug!("{}: waiting for input updates", candidate.id);

        let updates = match reader.read(None).await {
            Ok(updates) => updates,
            Err(e) => {
                if !handle_error(&candidate.id, e).await {
                    break;
                }

                continue;
            }
        };

        for update in updates {
            log::debug!("New update: {:#?}", update);

            let id = candidate.id.clone();

            match update {
                DeviceStateUpdate::ButtonDown(key) => {
                    if let Err(err) = device_plugin::key_down(id, key).await {
                        log::error!("Failed to send key_down: {}", err);
                    }
                }
                DeviceStateUpdate::ButtonUp(key) => {
                    if let Err(err) = device_plugin::key_up(id, key).await {
                        log::error!("Failed to send key_up: {}", err);
                    }
                }
                DeviceStateUpdate::EncoderDown(encoder) => {
                    if let Err(err) = device_plugin::encoder_down(id, encoder as u8).await {
                        log::error!("Failed to send encoder_down: {}", err);
                    }
                }
                DeviceStateUpdate::EncoderUp(encoder) => {
                    let encoder = encoder as usize;

                    if let Some(token) = encoder_repeat_tokens[encoder].take() {
                        token.cancel();
                    }

                    if let Err(err) = device_plugin::encoder_up(id, encoder as u8).await {
                        log::error!("Failed to send encoder_up: {}", err);
                    }
                }
                DeviceStateUpdate::EncoderTwist(encoder, val) => {
                    let encoder = encoder as usize;

                    if val == i8::MIN {
                        if let Some(token) = encoder_repeat_tokens[encoder].take() {
                            token.cancel();
                        }

                        continue;
                    }

                    if let Err(err) = device_plugin::encoder_change(id, encoder as u8, val as i16).await {
                        log::error!("Failed to send encoder_change: {}", err);
                    }

                    // Because the Stream Dock XL has rocker-style encoders, we make it repeat if the rocker
                    // is held for a bit. Works the same as holding a key on a regular keyboard. This way,
                    // if the user want to map one of those side rockers to brightness, they don't have to
                    // keep twisting the encoder to change brightness by a large amount: they can just hold it
                    // and wait for the value to start changing rapidly.
                    start_encoder_repeat(
                        candidate.id.clone(),
                        encoder,
                        val as i16,
                        &mut encoder_repeat_tokens[encoder],
                    );
                }
            }
        }
    }

    for token in encoder_repeat_tokens.into_iter().flatten() {
        token.cancel();
    }

    Ok(())
}

fn start_encoder_repeat(
    id: String,
    encoder: usize,
    value: i16,
    token_slot: &mut Option<CancellationToken>,
) {
    if let Some(token) = token_slot.take() {
        token.cancel();
    }

    let repeat_token = CancellationToken::new();
    *token_slot = Some(repeat_token.clone());

    tokio::spawn(encoder_repeat_task(
        id,
        encoder as u8,
        value,
        repeat_token,
    ));
}

async fn encoder_repeat_task(
    id: String,
    encoder: u8,
    value: i16,
    token: CancellationToken,
) {
    let initial_delay = time::sleep(Duration::from_secs(ENCODER_REPEAT_INITIAL_DELAY_SECS));
    tokio::pin!(initial_delay);

    tokio::select! {
        _ = token.cancelled() => return,
        _ = &mut initial_delay => {}
    }

    let mut interval = time::interval(Duration::from_millis(ENCODER_REPEAT_INTERVAL_MILLIS));
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = token.cancelled() => break,
            _ = interval.tick() => {
                if let Err(err) = device_plugin::encoder_change(id.clone(), encoder, value).await {
                    log::error!("Failed to send repeated encoder_change: {}", err);
                    break;
                }
            }
        }
    }
}

/// Sends periodic keepalives to the device to maintain connection
async fn keep_alive_task(candidate: &CandidateDevice, _device: Arc<Device>) -> Result<(), MirajazzError> {
    log::info!("Starting keep_alive loop for {}", candidate.id);

    let mut interval = time::interval(Duration::from_secs(HEART_BEAT_TIME));
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut last_wall_clock = SystemTime::now();
    let mut heartbeat_counter: u64 = 0;

    loop {
        interval.tick().await;

        let now = SystemTime::now();
        let gap = now
            .duration_since(last_wall_clock)
            .unwrap_or(Duration::from_secs(0));
        last_wall_clock = now;
        heartbeat_counter = heartbeat_counter.saturating_add(1);

        log::debug!("Heartbeat tick for {}: sending keep_alive", candidate.id);

        let device = {
            let devices_lock = DEVICES.read().await;

            match devices_lock.get(&candidate.id) {
                Some(device) => device.clone(),
                None => return Ok(()),
            }
        };

        if let Err(err) = device.keep_alive().await {
            if !handle_error(&candidate.id, err).await {
                break;
            }
        } else {
            log::debug!("Heartbeat acknowledged by device {}", candidate.id);

            // If wall-clock paused for a long time, we likely resumed from system sleep.
            // Re-arm software mode and replay cached state even if no disconnect/connect event was emitted.
            if gap > Duration::from_secs(HEART_BEAT_TIME * 2) {
                log::debug!(
                    "Detected long wall-clock gap ({:?}) for {}; forcing recycle to restore input path",
                    gap,
                    candidate.id
                );

                break;
            }

            // Safety net: periodically re-arm mode and repaint cached state to recover
            // from silent panel resets and mode drift.
            if heartbeat_counter % 6 == 0 {
                log::info!(
                    "Periodic mode/state refresh for {} on heartbeat #{}",
                    candidate.id,
                    heartbeat_counter
                );

                if let Err(err) = replay_cached_state(&candidate.id, device.as_ref()).await {
                    log::error!("Failed periodic cached-state replay for {}: {}", candidate.id, err);
                }
            }
        }
    }

    Ok(())
}

/// Handles different combinations of "set image" event, including clearing the specific buttons and whole device
pub async fn handle_set_image(device: &Device, evt: SetImageEvent) -> Result<(), MirajazzError> {
    match (evt.position, evt.image) {
        (Some(position), Some(image)) => {
            log::debug!("Setting image for button {}", position);

            // OpenDeck sends image as a data url, so parse it using a library
            let url = DataUrl::process(image.as_str()).unwrap(); // Isn't expected to fail, so unwrap it is
            let (body, _fragment) = url.decode_to_vec().unwrap(); // Same here

            // Allow only image/jpeg mime for now
            if url.mime_type().subtype != "jpeg" {
                log::error!("Incorrect mime type: {}", url.mime_type());

                return Ok(()); // Not a fatal error, enough to just log it
            }

            let format = image_format();
            let image = load_from_memory_with_format(body.as_slice(), image::ImageFormat::Jpeg)?;
            let image = image.resize_to_fill(format.size.0 as u32, format.size.1 as u32, FilterType::Lanczos3);

            device
                .set_button_image(
                    button_key(position),
                    format,
                    image,
                )
                .await?;
            device.flush().await?;
        }
        (Some(position), None) => {
            device
                .clear_button_image(button_key(position))
                .await?;
            device.flush().await?;
        }
        (None, None) => {
            device.clear_all_button_images().await?;
            device.flush().await?;
        }
        _ => {}
    }

    Ok(())
}
