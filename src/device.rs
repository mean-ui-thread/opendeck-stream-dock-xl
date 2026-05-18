use data_url::DataUrl;
use image::load_from_memory_with_format;
use mirajazz::{device::Device, error::MirajazzError, state::DeviceStateUpdate};
use openaction::{device_plugin, global_events::SetImageEvent};
use std::{sync::Arc, time::Duration};
use tokio::time::{self, MissedTickBehavior};
use tokio_util::sync::CancellationToken;

use crate::{
    DEVICES, TOKENS, led_config,
    inputs::button_key,
    mappings::{
        COL_COUNT, CandidateDevice, DEVICE_TYPE, ENCODER_COUNT, KEY_COUNT, ROW_COUNT,
        image_format,
    },
};

pub const HEART_BEAT_TIME: u64 = 10; // device powers off every 35 seconds seconds without receiving any command, so send keep-alive every 10 seconds just to be safe
const ENCODER_REPEAT_INITIAL_DELAY_SECS: u64 = 1;
const ENCODER_REPEAT_INTERVAL_MILLIS: u64 = 50;

/// Initializes a device and listens for events
pub async fn device_task(candidate: CandidateDevice, token: CancellationToken) {
    log::info!("Running device task for {:?}", candidate);

    // Wrap in a closure so we can use `?` operator
    let device = async || -> Result<Device, MirajazzError> {
        let device = connect(&candidate).await?;

        device.set_brightness(50).await?;
        device.clear_all_button_images().await?;
        device.flush().await?;

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

    tokio::select! {
        _ = device_events_task(&candidate, device.clone()) => {},
        _ = keep_alive_task(&candidate, device.clone()) => {},
        _ = token.cancelled() => {}
    };

    log::info!("Shutting down device {:?}", candidate);

    device.shutdown().await.ok();

    log::info!("Device task finished for {:?}", candidate);
}

/// Handles errors, returning true if should continue, returning false if an error is fatal
pub async fn handle_error(id: &String, err: MirajazzError) -> bool {
    log::error!("Device {} error: {}", id, err);

    // Some errors are not critical and can be ignored without sending disconnected event
    if matches!(err, MirajazzError::ImageError(_) | MirajazzError::BadData) {
        return true;
    }

    log::info!("Deregistering device {}", id);
    if let Err(err) = device_plugin::unregister_device(id.clone()).await {
        log::error!("Failed to deregister device {}: {}", id, err);
    }

    log::info!("Cancelling tasks for device {}", id);
    if let Some(token) = TOKENS.read().await.get(id) {
        token.cancel();
    }

    log::info!("Removing device {} from the list", id);
    DEVICES.write().await.remove(id);

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
        log::debug!("Reading updates...");

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

    loop {
        interval.tick().await;

        let devices_lock = DEVICES.read().await;
        let device = match devices_lock.get(&candidate.id) {
            Some(device) => device,
            None => return Ok(()),
        };

        if let Err(err) = device.keep_alive().await {
            drop(devices_lock);
            if !handle_error(&candidate.id, err).await {
                break;
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

            let image = load_from_memory_with_format(body.as_slice(), image::ImageFormat::Jpeg)?;

            device
                .set_button_image(
                    button_key(position),
                    image_format(),
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
