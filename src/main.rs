use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gtk4::prelude::*;
use gtk4::{gdk, glib};
use serde::{Deserialize, Serialize};

use gstreamer as gst;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gst::prelude::*;
use gst::prelude::DeviceExt;

#[derive(Clone)]
struct DeviceInfo {
    name: String,
    path: String,
    device: gst::Device,
}

struct Frame {
    width: i32,
    height: i32,
    stride: i32,
    data: Vec<u8>,
}

#[derive(Serialize, Deserialize)]
struct AppConfig {
    device_path: String,
}

struct AppState {
    devices: Vec<DeviceInfo>,
    pipeline: Option<gst::Pipeline>,
    config_path: PathBuf,
    dispatcher: FrameDispatcher,
}

#[derive(Clone)]
struct FrameDispatcher {
    context: glib::MainContext,
    picture: glib::SendWeakRef<gtk4::Picture>,
    aspect_frame: glib::SendWeakRef<gtk4::AspectFrame>,
    window: glib::SendWeakRef<gtk4::ApplicationWindow>,
    dropdown: glib::SendWeakRef<gtk4::DropDown>,
    vbox: glib::SendWeakRef<gtk4::Box>,
    last_size: Arc<Mutex<Option<(i32, i32)>>>,
}

impl FrameDispatcher {
    fn dispatch(&self, frame: Frame) {
        let picture = self.picture.clone();
        let aspect_frame = self.aspect_frame.clone();
        let window = self.window.clone();
        let dropdown = self.dropdown.clone();
        let vbox = self.vbox.clone();
        let last_size = self.last_size.clone();
        self.context.invoke(move || {
            let Some(picture) = picture.upgrade() else {
                return;
            };
            let Some(aspect_frame) = aspect_frame.upgrade() else {
                return;
            };
            if frame.width > 0 && frame.height > 0 {
                let bytes = glib::Bytes::from_owned(frame.data);
                let texture = gdk::MemoryTexture::new(
                    frame.width,
                    frame.height,
                    gdk::MemoryFormat::R8g8b8,
                    &bytes,
                    frame.stride as usize,
                );
                picture.set_paintable(Some(&texture));
                aspect_frame.set_ratio(frame.width as f32 / frame.height as f32);
                picture.set_size_request(frame.width, frame.height);
                aspect_frame.set_size_request(frame.width, frame.height);
                let mut last_size_ref = last_size.lock().unwrap();
                if last_size_ref.map_or(true, |(w, h)| w != frame.width || h != frame.height) {
                    *last_size_ref = Some((frame.width, frame.height));
                    let Some(window) = window.upgrade() else {
                        return;
                    };
                    let Some(dropdown) = dropdown.upgrade() else {
                        return;
                    };
                    let Some(vbox) = vbox.upgrade() else {
                        return;
                    };
                    let (drop_min_w, drop_nat_w, _, _) =
                        dropdown.measure(gtk4::Orientation::Horizontal, -1);
                    let (_drop_min_h, drop_nat_h, _, _) =
                        dropdown.measure(gtk4::Orientation::Vertical, -1);
                    let spacing = vbox.spacing();
                    let margin_h = vbox.margin_start() + vbox.margin_end();
                    let margin_v = vbox.margin_top() + vbox.margin_bottom();
                    let content_w = frame.width.max(drop_nat_w.max(drop_min_w));
                    let content_h = frame.height + drop_nat_h.max(0) + spacing;
                    let total_w = (content_w + margin_h).max(1);
                    let total_h = (content_h + margin_v).max(1);
                    window.set_default_size(total_w, total_h);
                    window.queue_resize();
                }
            }
        });
    }
}

fn main() {
    gst::init().expect("Failed to initialize GStreamer");

    let app = gtk4::Application::new(
        Some("com.example.webcam_viewer"),
        gio::ApplicationFlags::FLAGS_NONE,
    );
    app.connect_activate(build_ui);
    app.run();
}

fn build_ui(app: &gtk4::Application) {
    let devices = list_devices();
    let config_path = config_path();
    let saved = load_config(&config_path).map(|cfg| cfg.device_path);

    let selected_index = saved
        .as_ref()
        .and_then(|path| devices.iter().position(|d| d.path == *path))
        .or_else(|| if devices.is_empty() { None } else { Some(0) });

    if let Some(index) = selected_index {
        save_config(&config_path, &devices[index].path);
    }

    let window = gtk4::ApplicationWindow::new(app);
    window.set_title(Some("Webcam Viewer"));
    window.set_default_size(900, 700);

    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    let picture = gtk4::Picture::new();
    picture.set_hexpand(true);
    picture.set_vexpand(true);
    picture.set_keep_aspect_ratio(true);

    let aspect_frame = gtk4::AspectFrame::new(0.5, 0.5, 16.0 / 9.0, false);
    aspect_frame.set_hexpand(true);
    aspect_frame.set_vexpand(true);
    aspect_frame.set_child(Some(&picture));

    vbox.append(&aspect_frame);

    let device_names: Vec<String> = devices.iter().map(|d| d.name.clone()).collect();
    let device_name_refs: Vec<&str> = device_names.iter().map(|name| name.as_str()).collect();
    let dropdown = gtk4::DropDown::from_strings(&device_name_refs);
    dropdown.set_hexpand(true);

    if let Some(index) = selected_index {
        dropdown.set_selected(index as u32);
    }
    if devices.is_empty() {
        dropdown.set_sensitive(false);
    }

    vbox.append(&dropdown);

    window.set_child(Some(&vbox));
    window.present();

    let dispatcher = FrameDispatcher {
        context: glib::MainContext::default(),
        picture: glib::SendWeakRef::from(picture.downgrade()),
        aspect_frame: glib::SendWeakRef::from(aspect_frame.downgrade()),
        window: glib::SendWeakRef::from(window.downgrade()),
        dropdown: glib::SendWeakRef::from(dropdown.downgrade()),
        vbox: glib::SendWeakRef::from(vbox.downgrade()),
        last_size: Arc::new(Mutex::new(None)),
    };
    let state = Rc::new(RefCell::new(AppState {
        devices,
        pipeline: None,
        config_path,
        dispatcher,
    }));

    if let Some(index) = selected_index {
        start_pipeline(&state, index);
    }

    let state_clone = state.clone();
    dropdown.connect_selected_notify(move |dd| {
        let index = dd.selected() as usize;
        let (config_path, path) = {
            let guard = state_clone.borrow();
            let path = guard.devices.get(index).map(|d| d.path.clone());
            (guard.config_path.clone(), path)
        };
        if let Some(path) = path {
            save_config(&config_path, &path);
            start_pipeline(&state_clone, index);
        }
    });

    let state_clone = state.clone();
    window.connect_close_request(move |_| {
        stop_pipeline(&mut state_clone.borrow_mut());
        glib::Propagation::Proceed
    });
}

fn list_devices() -> Vec<DeviceInfo> {
    let monitor = gst::DeviceMonitor::new();
    monitor.add_filter(Some("Video/Source"), None::<&gst::Caps>);
    let _ = monitor.start();

    let devices = monitor.devices();
    monitor.stop();

    devices
        .into_iter()
        .filter_map(|device| {
            let props = device.properties();
            let path = props.as_ref().and_then(|s| {
                for key in ["device.path", "device", "api.v4l2.path", "object.path"] {
                    if let Ok(value) = s.get::<String>(key) {
                        return Some(value);
                    }
                }
                None
            });

            let path = path.map(|value| value.strip_prefix("v4l2:").unwrap_or(&value).to_string());

            path.map(|path| DeviceInfo {
                name: device.display_name().to_string(),
                path,
                device,
            })
        })
        .collect()
}

fn start_pipeline(state: &Rc<RefCell<AppState>>, index: usize) {
    let mut guard = state.borrow_mut();
    stop_pipeline(&mut guard);

    if let Some(device) = guard.devices.get(index) {
        if let Some(pipeline) = build_pipeline(device, guard.dispatcher.clone()) {
            match pipeline.set_state(gst::State::Playing) {
                Ok(_) => {
                    guard.pipeline = Some(pipeline);
                }
                Err(_) => {}
            }
        }
    }
}

fn stop_pipeline(state: &mut AppState) {
    if let Some(pipeline) = state.pipeline.take() {
        let _ = pipeline.set_state(gst::State::Null);
    }
}

fn build_pipeline(device: &DeviceInfo, dispatcher: FrameDispatcher) -> Option<gst::Pipeline> {
    let pipeline = gst::Pipeline::new();
    let src = if device.path.starts_with("/dev/video") {
        gst::ElementFactory::make("v4l2src").build().ok()?
    } else {
        device.device.create_element(None).ok()?
    };
    if src.has_property("device", None) {
        let _ = src.set_property("device", &device.path);
    }

    let decode = gst::ElementFactory::make("decodebin").build().ok()?;
    let queue = gst::ElementFactory::make("queue").build().ok()?;
    let convert = gst::ElementFactory::make("videoconvert").build().ok()?;
    let scale = gst::ElementFactory::make("videoscale").build().ok()?;
    let capsfilter = gst::ElementFactory::make("capsfilter").build().ok()?;
    let appsink = gst::ElementFactory::make("appsink").build().ok()?;
    let appsink = appsink.downcast::<gst_app::AppSink>().ok()?;

    let (src_w, src_h) = device_aspect(&device.device).unwrap_or((16, 9));
    let (target_w, target_h) = scaled_dimensions(src_w, src_h, 1280);

    let caps = gst::Caps::builder("video/x-raw")
        .field("format", &"RGB")
        .field("width", &target_w)
        .field("height", &target_h)
        .build();
    let _ = capsfilter.set_property("caps", &caps);

    pipeline
        .add_many(&[
            &src,
            &decode,
            &queue,
            &convert,
            &scale,
            &capsfilter,
            appsink.upcast_ref(),
        ])
        .ok()?;
    src.link(&decode).ok()?;
    gst::Element::link_many(&[&queue, &convert, &scale, &capsfilter, appsink.upcast_ref()]).ok()?;

    let queue_clone = queue.clone();
    decode.connect_pad_added(move |_, pad| {
        let Some(sink_pad) = queue_clone.static_pad("sink") else {
            return;
        };
        if sink_pad.is_linked() {
            return;
        }
        let _ = pad.link(&sink_pad);
    });

    let callbacks = gst_app::AppSinkCallbacks::builder()
        .new_sample(move |sink| {
            let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
            let caps = sample.caps().ok_or(gst::FlowError::Error)?;
            let info = gst_video::VideoInfo::from_caps(caps).map_err(|_| gst::FlowError::Error)?;

            let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
            let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;

            let stride = info.stride()[0];
            let data = map.as_slice();
            let mut owned = vec![0u8; data.len()];
            owned.copy_from_slice(data);

            let frame = Frame {
                width: info.width() as i32,
                height: info.height() as i32,
                stride: stride as i32,
                data: owned,
            };
            dispatcher.dispatch(frame);

            Ok(gst::FlowSuccess::Ok)
        })
        .build();
    appsink.set_callbacks(callbacks);

    Some(pipeline)
}

fn device_aspect(device: &gst::Device) -> Option<(i32, i32)> {
    let caps = device.caps()?;
    for idx in 0..caps.size() {
        if let Some(structure) = caps.structure(idx) {
            let width = structure.value("width").ok().and_then(|value| extract_i32(value));
            let height = structure.value("height").ok().and_then(|value| extract_i32(value));
            if let (Some(width), Some(height)) = (width, height) {
                return Some((width, height));
            }
        }
    }
    None
}

fn extract_i32(value: &glib::Value) -> Option<i32> {
    if let Ok(val) = value.get::<i32>() {
        return Some(val);
    }
    if let Ok(range) = value.get::<gst::IntRange<i32>>() {
        return Some(range.max());
    }
    if let Ok(list) = value.get::<gst::List>() {
        for item in list.iter() {
            if let Ok(val) = item.get::<i32>() {
                return Some(val);
            }
            if let Ok(range) = item.get::<gst::IntRange<i32>>() {
                return Some(range.max());
            }
        }
    }
    None
}

fn scaled_dimensions(width: i32, height: i32, long_side: i32) -> (i32, i32) {
    if width <= 0 || height <= 0 {
        return (long_side, long_side * 9 / 16);
    }
    if width >= height {
        let target_h = ((long_side as f32) * (height as f32) / (width as f32)).round() as i32;
        (long_side, target_h.max(2))
    } else {
        let target_w = ((long_side as f32) * (width as f32) / (height as f32)).round() as i32;
        (target_w.max(2), long_side)
    }
}

fn config_path() -> PathBuf {
    let mut base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    base.push("webcam_viewer");
    base.push("config.toml");
    base
}

fn load_config(path: &PathBuf) -> Option<AppConfig> {
    let contents = fs::read_to_string(path).ok()?;
    toml::from_str(&contents).ok()
}

fn save_config(path: &PathBuf, device_path: &str) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let config = AppConfig {
        device_path: device_path.to_string(),
    };
    if let Ok(contents) = toml::to_string(&config) {
        let _ = fs::write(path, contents);
    }
}
