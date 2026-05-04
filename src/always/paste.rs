use anyhow::{Context, Result};

pub fn copy_to_clipboard(text: String) -> Result<()> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    let mut pbcopy = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .context("Failed to run pbcopy")?;
    pbcopy
        .stdin
        .as_mut()
        .context("Failed to open pbcopy stdin")?
        .write_all(text.as_bytes())
        .context("Failed to write transcript to clipboard")?;
    let status = pbcopy.wait().context("Failed waiting for pbcopy")?;
    if !status.success() {
        anyhow::bail!("pbcopy failed");
    }
    Ok(())
}

pub fn paste_text(auto_enter: bool) -> Result<()> {
    use core_graphics::event::{CGEvent, CGEventFlags, CGEventTapLocation, CGKeyCode};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| anyhow::anyhow!("Failed to create CGEventSource"))?;

    // Simulate Cmd+V
    let v_keycode: CGKeyCode = 9; // 'v' key
    let key_down = CGEvent::new_keyboard_event(source.clone(), v_keycode, true)
        .map_err(|_| anyhow::anyhow!("Failed to create key down event"))?;
    key_down.set_flags(CGEventFlags::CGEventFlagCommand);
    key_down.post(CGEventTapLocation::HID);

    let key_up = CGEvent::new_keyboard_event(source.clone(), v_keycode, false)
        .map_err(|_| anyhow::anyhow!("Failed to create key up event"))?;
    key_up.set_flags(CGEventFlags::CGEventFlagCommand);
    key_up.post(CGEventTapLocation::HID);

    if auto_enter {
        std::thread::sleep(std::time::Duration::from_millis(20));
        let enter_keycode: CGKeyCode = 36; // Return key
        let enter_down = CGEvent::new_keyboard_event(source.clone(), enter_keycode, true)
            .map_err(|_| anyhow::anyhow!("Failed to create enter key down event"))?;
        enter_down.post(CGEventTapLocation::HID);

        let enter_up = CGEvent::new_keyboard_event(source, enter_keycode, false)
            .map_err(|_| anyhow::anyhow!("Failed to create enter key up event"))?;
        enter_up.post(CGEventTapLocation::HID);
    }

    Ok(())
}

pub fn paste(text: &str, auto_enter: bool) -> Result<()> {
    copy_to_clipboard(text.to_string())?;
    paste_text(auto_enter)?;
    Ok(())
}
