//! Key presses through `/dev/uinput`, for Wayland compositors without the
//! virtual-keyboard protocol (GNOME, KDE). The compositor sees a keyboard and
//! decodes it like the user's own, so no helper daemon is needed. Access comes
//! from the `60-always-uinput.rules` udev rule the installers set up.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use parking_lot::Mutex;

/// evdev codes.
pub const KEY_ENTER: u16 = 28;
pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_V: u16 = 47;

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;

const UI_SET_EVBIT: u64 = 0x4004_5564;
const UI_SET_KEYBIT: u64 = 0x4004_5565;
const UI_DEV_SETUP: u64 = 0x405c_5503;
const UI_DEV_CREATE: u64 = 0x5501;

/// Time for the compositor to adopt a new keyboard; keys sent sooner are lost.
const DEVICE_SETTLE: Duration = Duration::from_millis(300);
const KEY_GAP: Duration = Duration::from_millis(3);

#[repr(C)]
struct UinputSetup {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
    name: [u8; 80],
    ff_effects_max: u32,
}

#[repr(C)]
struct InputEvent {
    tv_sec: i64,
    tv_usec: i64,
    kind: u16,
    code: u16,
    value: i32,
}

/// Kept open for the daemon's lifetime: closing it destroys the keyboard.
static DEVICE: Mutex<Option<File>> = Mutex::new(None);

fn ioctl_int(file: &File, request: u64, value: i32) -> Result<()> {
    // SAFETY: the fd is open and these requests take an int by value.
    if unsafe { libc::ioctl(file.as_raw_fd(), request as _, value) } < 0 {
        bail!(
            "uinput ioctl {request:#x}: {}",
            std::io::Error::last_os_error()
        );
    }
    Ok(())
}

fn create_device() -> Result<File> {
    let file = OpenOptions::new()
        .write(true)
        .open("/dev/uinput")
        .context("open /dev/uinput")?;
    ioctl_int(&file, UI_SET_EVBIT, i32::from(EV_KEY))?;
    for key in [KEY_ENTER, KEY_LEFTCTRL, KEY_LEFTSHIFT, KEY_V] {
        ioctl_int(&file, UI_SET_KEYBIT, i32::from(key))?;
    }
    let mut name = [0u8; 80];
    name[..15].copy_from_slice(b"Always keyboard");
    let setup = UinputSetup {
        bustype: 0x03, // BUS_USB
        vendor: 0x1209,
        product: 0xa1a7,
        version: 1,
        name,
        ff_effects_max: 0,
    };
    // SAFETY: `setup` is the exact layout UI_DEV_SETUP reads and outlives the call.
    if unsafe { libc::ioctl(file.as_raw_fd(), UI_DEV_SETUP as _, &setup) } < 0 {
        bail!("uinput setup: {}", std::io::Error::last_os_error());
    }
    // SAFETY: UI_DEV_CREATE takes no argument.
    if unsafe { libc::ioctl(file.as_raw_fd(), UI_DEV_CREATE as _, 0) } < 0 {
        bail!("uinput create: {}", std::io::Error::last_os_error());
    }
    std::thread::sleep(DEVICE_SETTLE);
    Ok(file)
}

fn emit(mut file: &File, kind: u16, code: u16, value: i32) -> Result<()> {
    let event = InputEvent {
        tv_sec: 0,
        tv_usec: 0,
        kind,
        code,
        value,
    };
    // SAFETY: `InputEvent` is repr(C), plain data, without padding.
    let bytes = unsafe {
        std::slice::from_raw_parts(
            (&raw const event).cast::<u8>(),
            std::mem::size_of::<InputEvent>(),
        )
    };
    file.write_all(bytes).context("write uinput event")
}

fn set_key(file: &File, key: u16, down: bool) -> Result<()> {
    emit(file, EV_KEY, key, i32::from(down))?;
    emit(file, EV_SYN, 0, 0)?;
    std::thread::sleep(KEY_GAP);
    Ok(())
}

/// Press `keys` in order, then release them in reverse.
pub fn chord(keys: &[u16]) -> Result<()> {
    let mut device = DEVICE.lock();
    if device.is_none() {
        *device = Some(create_device()?);
    }
    let file = device.as_ref().expect("device was just created");
    let sent = keys
        .iter()
        .try_for_each(|&key| set_key(file, key, true))
        .and_then(|()| {
            keys.iter()
                .rev()
                .try_for_each(|&key| set_key(file, key, false))
        });
    if sent.is_err() {
        *device = None;
    }
    sent
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_abi_sizes() {
        assert_eq!(std::mem::size_of::<UinputSetup>(), 92);
        assert_eq!(std::mem::size_of::<InputEvent>(), 24);
    }
}
