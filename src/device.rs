use std::convert::TryFrom;
use std::path::Path;
use std::sync::Arc;
use std::{io, mem};

use crate::control;
use crate::pselect::FdSet;
use crate::v4l2;
use crate::v4l2::videodev::v4l2_ext_controls;
use crate::v4l_sys::*;
use crate::{capability::Capabilities, control::Control};

/// Linux capture device abstraction
pub struct Device {
    /// Raw handle
    handle: Arc<Handle>,
}

impl Device {
    /// Returns a capture device by index
    ///
    /// Devices are usually enumerated by the system.
    /// An index of zero thus represents the first device the system got to know about.
    ///
    /// # Arguments
    ///
    /// * `index` - Index (0: first, 1: second, ..)
    ///
    /// # Example
    ///
    /// ```
    /// use v4l::device::Device;
    /// let dev = Device::new(0);
    /// ```
    pub fn new(index: usize) -> io::Result<Self> {
        let path = format!("{}{}", "/dev/video", index);
        let fd = v4l2::open(&path, libc::O_RDWR | libc::O_NONBLOCK)?;

        if fd == -1 {
            return Err(io::Error::last_os_error());
        }

        let mut fd_set = FdSet::new();
        fd_set.set(fd);

        Ok(Device {
            handle: Arc::new(Handle { fd, fd_set }),
        })
    }

    /// Returns a capture device by path
    ///
    /// Linux device nodes are usually found in /dev/videoX or /sys/class/video4linux/videoX.
    ///
    /// # Arguments
    ///
    /// * `path` - Path (e.g. "/dev/video0")
    ///
    /// # Example
    ///
    /// ```
    /// use v4l::device::Device;
    /// let dev = Device::with_path("/dev/video0");
    /// ```
    pub fn with_path<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        let fd = v4l2::open(&path, libc::O_RDWR | libc::O_NONBLOCK)?;

        if fd == -1 {
            return Err(io::Error::last_os_error());
        }

        let mut fd_set = FdSet::new();
        fd_set.set(fd);

        Ok(Device {
            handle: Arc::new(Handle { fd, fd_set }),
        })
    }

    /// Returns the raw device handle
    pub fn handle(&self) -> Arc<Handle> {
        self.handle.clone()
    }

    /// Returns video4linux framework defined information such as card, driver, etc.
    pub fn query_caps(&self) -> io::Result<Capabilities> {
        unsafe {
            let mut v4l2_caps: v4l2_capability = mem::zeroed();
            v4l2::ioctl(
                self.handle().fd(),
                v4l2::vidioc::VIDIOC_QUERYCAP,
                &mut v4l2_caps as *mut _ as *mut std::os::raw::c_void,
            )?;

            Ok(Capabilities::from(v4l2_caps))
        }
    }

    /// Returns the supported controls for a device such as gain, focus, white balance, etc.
    pub fn query_controls(&self) -> io::Result<Vec<control::Description>> {
        let mut controls = Vec::new();
        unsafe {
            let mut v4l2_ctrl: v4l2_queryctrl = mem::zeroed();

            loop {
                v4l2_ctrl.id |= V4L2_CTRL_FLAG_NEXT_CTRL;
                v4l2_ctrl.id |= V4L2_CTRL_FLAG_NEXT_COMPOUND;
                match v4l2::ioctl(
                    self.handle().fd(),
                    v4l2::vidioc::VIDIOC_QUERYCTRL,
                    &mut v4l2_ctrl as *mut _ as *mut std::os::raw::c_void,
                ) {
                    Ok(_) => {
                        // get the basic control information
                        let mut control = control::Description::from(v4l2_ctrl);

                        // if this is a menu control, enumerate its items
                        if control.typ == control::Type::Menu
                            || control.typ == control::Type::IntegerMenu
                        {
                            let mut items = Vec::new();

                            let mut v4l2_menu: v4l2_querymenu = mem::zeroed();
                            v4l2_menu.id = v4l2_ctrl.id;

                            for i in (v4l2_ctrl.minimum..=v4l2_ctrl.maximum)
                                .step_by(v4l2_ctrl.step as usize)
                            {
                                v4l2_menu.index = i as u32;
                                let res = v4l2::ioctl(
                                    self.handle().fd(),
                                    v4l2::vidioc::VIDIOC_QUERYMENU,
                                    &mut v4l2_menu as *mut _ as *mut std::os::raw::c_void,
                                );

                                // BEWARE OF DRAGONS!
                                // The API docs [1] state VIDIOC_QUERYMENU should may return EINVAL
                                // for some indices between minimum and maximum when an item is not
                                // supported by a driver.
                                //
                                // I have no idea why it is advertised in the first place then, but
                                // have seen this happen with a Logitech C920 HD Pro webcam.
                                // In case of errors, let's just skip the offending index.
                                //
                                // [1] https://github.com/torvalds/linux/blob/master/Documentation/userspace-api/media/v4l/vidioc-queryctrl.rst#description
                                if res.is_err() {
                                    continue;
                                }

                                let item =
                                    control::MenuItem::try_from((control.typ, v4l2_menu)).unwrap();
                                items.push((v4l2_menu.index, item));
                            }

                            control.items = Some(items);
                        }

                        controls.push(control);
                    }
                    Err(e) => {
                        if controls.is_empty() || e.kind() != io::ErrorKind::InvalidInput {
                            return Err(e);
                        } else {
                            break;
                        }
                    }
                }
            }
        }

        Ok(controls)
    }

    /// Returns the control value for an ID
    ///
    /// # Arguments
    ///
    /// * `id` - Control identifier
    pub fn control(&self, id: u32) -> io::Result<Control> {
        unsafe {
            let mut queryctrl: v4l2_queryctrl = mem::zeroed();
            queryctrl.id = id;
            v4l2::ioctl(
                self.handle().fd(),
                v4l2::vidioc::VIDIOC_QUERYCTRL,
                &mut queryctrl as *mut _ as *mut std::os::raw::c_void,
            )?;

            // determine the control type
            let description = control::Description::from(queryctrl);

            // query the actual control value
            let mut v4l2_ctrl: v4l2_control = mem::zeroed();
            v4l2_ctrl.id = id;
            v4l2::ioctl(
                self.handle().fd(),
                v4l2::vidioc::VIDIOC_G_CTRL,
                &mut v4l2_ctrl as *mut _ as *mut std::os::raw::c_void,
            )?;

            let value = match description.typ {
                control::Type::Integer | control::Type::Integer64 => {
                    control::Value::Integer(v4l2_ctrl.value as i64)
                }
                control::Type::Boolean => control::Value::Boolean(v4l2_ctrl.value == 1),
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        "cannot handle control type",
                    ))
                }
            };

            Ok(Control { id, value })
        }
    }

    /// Modifies the control value
    ///
    /// # Arguments
    ///
    /// * `ctrl` - Control to be set
    pub fn set_control(&self, ctrl: Control) -> io::Result<()> {
        self.set_controls(vec![ctrl])
    }

    /// Modifies the control values atomically
    ///
    /// # Arguments
    ///
    /// * `ctrls` - Vec of the controls to be set
    pub fn set_controls(&self, ctrls: Vec<Control>) -> io::Result<()> {
        unsafe {
            let mut controls: v4l2_ext_controls = mem::zeroed();
            let mut control_list: Vec<v4l2_ext_control> = vec![];
            let mut class: Option<u32> = None;

            controls.count = ctrls.len() as u32;

            if controls.count == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "ctrls cannot be empty",
                ));
            }

            for ref ctrl in ctrls {
                let mut control: v4l2_ext_control = mem::zeroed();

                control.id = ctrl.id;
                class = match class {
                    Some(c) => {
                        if c != (control.id & 0xFFFF0000) {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "All controls must be in the same class",
                            ));
                        } else {
                            Some(c)
                        }
                    }
                    None => Some(control.id & 0xFFFF0000),
                };

                match ctrl.value {
                    control::Value::None => {}
                    control::Value::Integer(val) => {
                        control.__bindgen_anon_1.value64 = val;
                        control.size = std::mem::size_of::<i64>() as u32;
                    }
                    control::Value::Boolean(val) => {
                        control.__bindgen_anon_1.value64 = val as i64;
                        control.size = std::mem::size_of::<i64>() as u32;
                    }
                    control::Value::String(ref val) => {
                        control.__bindgen_anon_1.string = val.as_ptr() as *mut i8;
                        control.size = val.len() as u32;
                    }
                    control::Value::CompoundU8(ref val) => {
                        control.__bindgen_anon_1.p_u8 = val.as_ptr() as *mut u8;
                        control.size = (val.len() * std::mem::size_of::<u8>()) as u32;
                    }
                    control::Value::CompoundU16(ref val) => {
                        control.__bindgen_anon_1.p_u16 = val.as_ptr() as *mut u16;
                        control.size = (val.len() * std::mem::size_of::<u16>()) as u32;
                    }
                    control::Value::CompoundU32(ref val) => {
                        control.__bindgen_anon_1.p_u32 = val.as_ptr() as *mut u32;
                        control.size = (val.len() * std::mem::size_of::<u32>()) as u32;
                    }
                    control::Value::CompoundPtr(ref val) => {
                        control.__bindgen_anon_1.ptr = val.as_ptr() as *mut std::os::raw::c_void;
                        control.size = (val.len() * std::mem::size_of::<u8>()) as u32;
                    }
                };

                control_list.push(control);
            }

            if let Some(class) = class {
                controls.which = class;
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "failed to determine control class",
                ));
            };

            controls.controls = control_list.as_mut_ptr() as *mut v4l2_ext_control;

            v4l2::ioctl(
                self.handle().fd(),
                v4l2::vidioc::VIDIOC_S_EXT_CTRLS,
                &mut controls as *mut _ as *mut std::os::raw::c_void,
            )
        }
    }
}

impl io::Read for Device {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        unsafe {
            let ret = libc::read(
                self.handle().fd(),
                buf.as_mut_ptr() as *mut std::os::raw::c_void,
                buf.len(),
            );
            match ret {
                -1 => Err(io::Error::last_os_error()),
                ret => Ok(ret as usize),
            }
        }
    }
}

impl io::Write for Device {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        unsafe {
            let ret = libc::write(
                self.handle().fd(),
                buf.as_ptr() as *const std::os::raw::c_void,
                buf.len(),
            );

            match ret {
                -1 => Err(io::Error::last_os_error()),
                ret => Ok(ret as usize),
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        // write doesn't use a buffer, so it effectively flushes with each call
        // therefore, we don't have anything to flush later
        Ok(())
    }
}

/// Device handle for low-level access.
///
/// Acquiring a handle facilitates (possibly mutating) interactions with the device.
pub struct Handle {
    fd: std::os::raw::c_int,
    fd_set: FdSet,
}

impl Handle {
    /// Returns the raw file descriptor
    pub fn fd(&self) -> std::os::raw::c_int {
        self.fd
    }

    /// Returns the raw file descriptor set
    pub fn fd_set(&self) -> FdSet {
        self.fd_set
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        v4l2::close(self.fd).unwrap();
    }
}
