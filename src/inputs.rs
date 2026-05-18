use mirajazz::{error::MirajazzError, types::DeviceInput};

use crate::mappings::{ENCODER_COUNT, KEY_COUNT};

pub fn process_input(input: u8, state: u8) -> Result<DeviceInput, MirajazzError> {
    log::debug!("Processing input: {input}=0x{input:02x}=0b{input:08b}, {state}");

    match input as usize {
        (0x00..=0x20) => read_button_press(input, state),
        0x21 | 0x23 | 0x24 | 0x26 => read_encoder_value(input, state),
        0x22 | 0x25 => read_encoder_press(input, state),
        _ => Err(MirajazzError::BadData),
    }
}

fn read_button_states(states: &[u8]) -> Vec<bool> {
    let mut bools = vec![];

    for i in 0..KEY_COUNT {
        bools.push(states[i + 1] != 0);
    }

    bools
}

pub fn button_key(key: u8) -> u8 {
    if key < KEY_COUNT as u8 {
        // From https://github.com/MiraboxSpace/StreamDock-Device-SDK/blob/bc08f2cffceb03b01adda185d056c8e8c824a480/CPP-SDK/src/HotspotDevice/StreamDockXL/streamdockXL.cpp#L65-L68
        [
            0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F,
            0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17,
            0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F,
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
        ][key as usize]
    } else {
        key
    }
}


fn read_button_press(input: u8, state: u8) -> Result<DeviceInput, MirajazzError> {
    let mut button_states = vec![0x01];
    button_states.extend(vec![0u8; KEY_COUNT + 1]);

    if input == 0 {
        return Ok(DeviceInput::ButtonStateChange(read_button_states(
            &button_states,
        )));
    }

    let pressed_index: usize = match input {
        0x00..=0x20 => input as usize,
        _ => return Err(MirajazzError::BadData),
    };

    button_states[pressed_index] = state;

    Ok(DeviceInput::ButtonStateChange(read_button_states(
        &button_states,
    )))
}

fn read_encoder_value(input: u8, state: u8) -> Result<DeviceInput, MirajazzError> {
    let (encoder, value): (usize, i8) = match input {
        // Encoder 0 (left side)
        0x21 => (0, 1 * state as i8), // clockwise
        0x23 => (0, -1 * state as i8), // counterclockwise
        // Encoder 1 (right side)
        0x24 => (1, -1 * state as i8), // counterclockwise
        0x26 => (1, 1 * state as i8), // clockwise
        _ => return Err(MirajazzError::BadData),
    };

    let mut encoder_values = vec![0i8; ENCODER_COUNT];
    encoder_values[encoder] = if state == 0 { i8::MIN } else { value };

    Ok(DeviceInput::EncoderTwist(encoder_values))
}

fn read_encoder_press(input: u8, state: u8) -> Result<DeviceInput, MirajazzError> {
    let mut encoder_states = vec![false; ENCODER_COUNT];

    let encoder: usize = match input {
        0x22 => 0, // Encoder 0 (left side).
        0x25 => 1, // Encoder 1 (right side)
        _ => return Err(MirajazzError::BadData),
    };

    // state>0 means EncoderDown, state=0 means EncoderUp.
    encoder_states[encoder] = state > 0;
    Ok(DeviceInput::EncoderStateChange(encoder_states))
}