use gtk::{prelude::*, glib, Application, ApplicationWindow, Button, Text};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::sync::mpsc;
use std::cell::RefCell;
use std::rc::Rc;

mod config;
mod rdp;
mod ws; // Add websocket compatibility module

use crate::rdp::{RdpInputEvent, RdpOutputEvent, RdpClient, DvcPipeProxyFactory};
use crate::config::{Config, ClipboardType, Destination};

const APP_ID: &str = "org.gtk_rs.IronTsc";
const CONFIG_FILE: &str = "config.json";

/// Convert Windows NT status code to user-friendly error message
fn get_friendly_error_message(ntstatus_code: u32) -> Option<(&'static str, &'static str)> {
    // Returns (title, detailed_message)
    match ntstatus_code {
        0xc000006d => Some((
            "Login Failed - Invalid Credentials",
            "The username or password is incorrect.\n\n\
            Please check:\n\
            • Username is spelled correctly\n\
            • Password is correct\n\
            • Domain name (if required)\n\
            • Caps Lock is not on"
        )),
        0xc0000234 => Some((
            "Account Locked Out",
            "Your account has been locked due to too many failed login attempts.\n\n\
            Please:\n\
            • Wait 30 minutes for automatic unlock, or\n\
            • Contact your system administrator to unlock the account\n\n\
            On the RDP server, run: net user USERNAME /active:yes"
        )),
        0xc0000071 => Some((
            "Password Expired",
            "Your password has expired and must be changed.\n\n\
            Please log in to the RDP server directly (console access) and change your password."
        )),
        0xc0000072 => Some((
            "Account Disabled",
            "This user account is disabled.\n\n\
            Contact your system administrator to enable the account.\n\n\
            On the RDP server, run: net user USERNAME /active:yes"
        )),
        0xc000006f => Some((
            "Account Restriction",
            "Your account has restrictions that prevent you from logging in at this time.\n\n\
            Possible causes:\n\
            • Time-based login restrictions\n\
            • Workstation login restrictions\n\
            • Account is only allowed to log in at certain times"
        )),
        0xc0000070 => Some((
            "Invalid Workstation",
            "You are not allowed to log in from this computer.\n\n\
            Contact your system administrator to grant access from this workstation."
        )),
        0xc0000193 => Some((
            "Account Expired",
            "This user account has expired.\n\n\
            Contact your system administrator to reactivate the account."
        )),
        0xc0000064 => Some((
            "User Does Not Exist",
            "The specified user account does not exist.\n\n\
            Please check:\n\
            • Username is spelled correctly\n\
            • Account exists on the RDP server"
        )),
        0xc000006a => Some((
            "Wrong Password",
            "The password is incorrect.\n\n\
            Please check:\n\
            • Password is correct\n\
            • Caps Lock is not on\n\
            • Correct keyboard layout is selected"
        )),
        0xc0000224 => Some((
            "Password Must Change",
            "You must change your password before logging in.\n\n\
            This is typically required on first login or after a password reset."
        )),
        0xc0000413 => Some((
            "Authentication Firewall Restriction",
            "A firewall restriction prevented authentication.\n\n\
            Contact your system administrator to check firewall rules."
        )),
        _ => None,
    }
}

/// Format error message with NT status code information
fn format_rdp_error(error: &impl std::fmt::Debug) -> (String, String) {
    let error_str = format!("{:?}", error);
    
    // Try to extract NStatusCode from the error
    if let Some(start) = error_str.find("NStatusCode(0x") {
        if let Some(end) = error_str[start..].find(')') {
            let code_str = &error_str[start + 14..start + end]; // Skip "NStatusCode(0x"
            if let Ok(code) = u32::from_str_radix(code_str, 16) {
                if let Some((title, message)) = get_friendly_error_message(code) {
                    return (
                        title.to_string(),
                        format!("{}\n\nTechnical details: Error code 0x{:08x}", message, code)
                    );
                }
            }
        }
    }
    
    // Check for common error patterns
    if error_str.contains("CredSSP") {
        return (
            "Authentication Failed".to_string(),
            format!("Network Level Authentication (CredSSP) failed.\n\n\
                This usually means invalid credentials or account issues.\n\n\
                Technical details:\n{:?}", error)
        );
    }
    
    if error_str.contains("TLS") || error_str.contains("SSL") {
        return (
            "Secure Connection Failed".to_string(),
            format!("Failed to establish a secure (TLS) connection.\n\n\
                Please check:\n\
                • Server certificate is valid\n\
                • Server supports TLS\n\n\
                Technical details:\n{:?}", error)
        );
    }
    
    if error_str.contains("TCP") || error_str.contains("Connection refused") {
        return (
            "Cannot Connect to Server".to_string(),
            format!("Failed to connect to the RDP server.\n\n\
                Please check:\n\
                • Server address is correct\n\
                • Server is running and reachable\n\
                • Port 3389 is open\n\
                • Network/firewall settings\n\n\
                Technical details:\n{:?}", error)
        );
    }
    
    // Default generic error
    (
        "RDP Connection Failed".to_string(),
        format!("An error occurred while connecting to the RDP server.\n\n\
            Technical details:\n{:?}", error)
    )
}

fn create_rdp_config(server: &str, username: &str, domain: &str, password: &str) -> Config {
    use ironrdp::connector;
    use ironrdp::pdu::rdp::capability_sets::MajorPlatformType;
    use ironrdp::pdu::rdp::client_info::{PerformanceFlags, TimezoneInfo};
    
    let destination = Destination::new(server.to_string()).unwrap();
    
    let connector_config = connector::Config {
        credentials: connector::Credentials::UsernamePassword {
            username: username.to_string(),
            password: password.to_string(),
        },
        domain: if domain.is_empty() { None } else { Some(domain.to_string()) },
        client_name: "IronTSC".to_string(),
        desktop_size: connector::DesktopSize { width: 1024, height: 768 },
        enable_server_pointer: true,
        pointer_software_rendering: false,
        autologon: true,
        desktop_scale_factor: 1,
        enable_tls: true,
        enable_credssp: true,
        keyboard_type: ironrdp::pdu::gcc::KeyboardType::IbmEnhanced,
        keyboard_subtype: 0,
        keyboard_functional_keys_count: 12,
        client_build: 1,
        client_dir: "".to_string(),
        platform: MajorPlatformType::UNIX,
        keyboard_layout: 0,
        ime_file_name: "".to_string(),
        dig_product_id: "".to_string(),
        hardware_id: None,
        bitmap: None,
        request_data: None,
        enable_audio_playback: false,
        performance_flags: PerformanceFlags::empty(),
        license_cache: None,
        timezone_info: TimezoneInfo::default(),
    };

    Config {
        log_file: None,
        gw: None,
        destination,
        connector: connector_config,
        clipboard_type: ClipboardType::Default,
        rdcleanpath: None,
        dvc_pipe_proxies: Vec::new(),
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct AppConfig {
    server: String,
    username: String,
    domain: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            server: String::new(),
            username: String::new(),
            domain: String::new(),
        }
    }
}

impl AppConfig {
    fn config_dir() -> Option<PathBuf> {
        dirs::config_dir().map(|p| p.join("irontsc"))
    }

    fn load() -> Self {
        let dir = match Self::config_dir() {
            Some(d) => d,
            None => return Self::default(),
        };
        let path = dir.join(CONFIG_FILE);
        if let Ok(contents) = std::fs::read_to_string(&path) {
            if let Ok(cfg) = serde_json::from_str::<AppConfig>(&contents) {
                return cfg;
            }
        }
        Self::default()
    }

    fn save(&self) {
        if let Some(dir) = Self::config_dir() {
            let _ = std::fs::create_dir_all(&dir);
            let path = dir.join(CONFIG_FILE);
            if let Ok(json) = serde_json::to_string_pretty(self) {
                let _ = std::fs::write(path, json);
            }
        }
    }
}

// GTK RDP Widget Implementation
struct GtkRdpWidget {
    drawing_area: gtk::DrawingArea,
    buffer: Rc<RefCell<Vec<u32>>>,
    cairo_buffer: Rc<RefCell<Vec<u32>>>,  // Cached converted buffer
    buffer_size: Rc<RefCell<(u16, u16)>>,
    input_event_sender: mpsc::UnboundedSender<RdpInputEvent>,
    input_database: Rc<RefCell<ironrdp::input::Database>>,
}

impl GtkRdpWidget {
    fn new(input_event_sender: mpsc::UnboundedSender<RdpInputEvent>) -> Self {
        let drawing_area = gtk::DrawingArea::new();
        drawing_area.set_hexpand(true);
        drawing_area.set_vexpand(true);
        drawing_area.set_can_focus(true);
        drawing_area.set_focusable(true);

        let buffer = Rc::new(RefCell::new(Vec::new()));
        let cairo_buffer = Rc::new(RefCell::new(Vec::new()));
        let buffer_size = Rc::new(RefCell::new((0u16, 0u16)));
        let input_database = Rc::new(RefCell::new(ironrdp::input::Database::new()));

        let widget = Self {
            drawing_area: drawing_area.clone(),
            buffer: buffer.clone(),
            cairo_buffer: cairo_buffer.clone(),
            buffer_size: buffer_size.clone(),
            input_event_sender: input_event_sender.clone(),
            input_database: input_database.clone(),
        };

        // Set up drawing
        let cairo_buffer_draw = cairo_buffer.clone();
        let buffer_size_draw = buffer_size.clone();
        drawing_area.set_draw_func(move |_drawing_area, cr, width, height| {
            let cairo_buffer = cairo_buffer_draw.borrow();
            let (buf_width, buf_height) = *buffer_size_draw.borrow();
            
            if cairo_buffer.is_empty() || buf_width == 0 || buf_height == 0 {
                // Draw a placeholder background
                cr.set_source_rgb(0.1, 0.1, 0.2);
                cr.rectangle(0.0, 0.0, width as f64, height as f64);
                let _ = cr.fill();

                // Show "Connecting..." message
                cr.set_source_rgb(0.8, 0.8, 0.8);
                let text = "Connecting to RDP server...";
                let text_extents = cr.text_extents(text).unwrap_or_else(|_| {
                    gtk::cairo::TextExtents::new(0.0, 0.0, 100.0, 20.0, 0.0, 0.0)
                });
                let x = (width as f64 - text_extents.width()) / 2.0;
                let y = (height as f64) / 2.0;
                cr.move_to(x, y);
                let _ = cr.show_text(text);
                return;
            }

            // Use the pre-converted Cairo buffer directly
            // Convert to bytes view without copying
            let buffer_bytes: &[u8] = unsafe {
                std::slice::from_raw_parts(
                    cairo_buffer.as_ptr() as *const u8,
                    cairo_buffer.len() * 4
                )
            };

            // Create Cairo surface from the cached buffer (zero-copy reference)
            if let Ok(surface) = gtk::cairo::ImageSurface::create_for_data(
                buffer_bytes.to_vec(), // Cairo needs ownership, so copy here
                gtk::cairo::Format::ARgb32,
                buf_width as i32,
                buf_height as i32,
                buf_width as i32 * 4,
            ) {
                // Scale the image to fit the drawing area
                let scale_x = width as f64 / buf_width as f64;
                let scale_y = height as f64 / buf_height as f64;
                
                cr.save().unwrap();
                cr.scale(scale_x, scale_y);
                cr.set_source_surface(&surface, 0.0, 0.0).unwrap();
                cr.paint().unwrap();
                cr.restore().unwrap();
            }
        });

        // Set up input event handlers
        widget.setup_input_handlers();
        
        widget
    }

    fn setup_input_handlers(&self) {
        // Keyboard events
        let key_controller = gtk::EventControllerKey::new();
        let input_sender_key = self.input_event_sender.clone();
        let input_database_key = self.input_database.clone();
        
        let input_sender_key_pressed = input_sender_key.clone();
        let input_database_key_pressed = input_database_key.clone();
        key_controller.connect_key_pressed(move |_, _key, keycode, _modifiers| {
            if let Some(scancode) = Self::keycode_to_scancode(keycode) {
                let operation = ironrdp::input::Operation::KeyPressed(scancode);
                let input_events = input_database_key_pressed.borrow_mut().apply(std::iter::once(operation));
                Self::send_fast_path_events(&input_sender_key_pressed, input_events);
            }
            glib::Propagation::Proceed
        });

        key_controller.connect_key_released(move |_, _key, keycode, _modifiers| {
            if let Some(scancode) = Self::keycode_to_scancode(keycode) {
                let operation = ironrdp::input::Operation::KeyReleased(scancode);
                let input_events = input_database_key.borrow_mut().apply(std::iter::once(operation));
                Self::send_fast_path_events(&input_sender_key, input_events);
            }
        });

        self.drawing_area.add_controller(key_controller);

        // Mouse events
        let click_controller = gtk::GestureClick::new();
        let input_sender_click = self.input_event_sender.clone();
        let input_database_click = self.input_database.clone();
        
        let input_sender_click_pressed = input_sender_click.clone();
        let input_database_click_pressed = input_database_click.clone();
        click_controller.connect_pressed(move |gesture, _n_press, _x, _y| {
            let button = gesture.current_button();
            let mouse_button = Self::gtk_button_to_rdp_button(button);
            if let Some(mouse_button) = mouse_button {
                let operation = ironrdp::input::Operation::MouseButtonPressed(mouse_button);
                let input_events = input_database_click_pressed.borrow_mut().apply(std::iter::once(operation));
                Self::send_fast_path_events(&input_sender_click_pressed, input_events);
            }
        });

        click_controller.connect_released(move |gesture, _n_press, _x, _y| {
            let button = gesture.current_button();
            let mouse_button = Self::gtk_button_to_rdp_button(button);
            if let Some(mouse_button) = mouse_button {
                let operation = ironrdp::input::Operation::MouseButtonReleased(mouse_button);
                let input_events = input_database_click.borrow_mut().apply(std::iter::once(operation));
                Self::send_fast_path_events(&input_sender_click, input_events);
            }
        });

        self.drawing_area.add_controller(click_controller);

        // Mouse motion
        let motion_controller = gtk::EventControllerMotion::new();
        let input_sender_motion = self.input_event_sender.clone();
        let input_database_motion = self.input_database.clone();
        let buffer_size_motion = self.buffer_size.clone();
        let drawing_area_motion = self.drawing_area.clone();
        
        motion_controller.connect_motion(move |_, x, y| {
            let allocation = drawing_area_motion.allocation();
            let (buf_width, buf_height) = *buffer_size_motion.borrow();
            
            if buf_width > 0 && buf_height > 0 {
                let rdp_x = (x / allocation.width() as f64 * buf_width as f64) as u16;
                let rdp_y = (y / allocation.height() as f64 * buf_height as f64) as u16;
                
                let operation = ironrdp::input::Operation::MouseMove(ironrdp::input::MousePosition { x: rdp_x, y: rdp_y });
                let input_events = input_database_motion.borrow_mut().apply(std::iter::once(operation));
                Self::send_fast_path_events(&input_sender_motion, input_events);
            }
        });

        self.drawing_area.add_controller(motion_controller);
    }

    fn keycode_to_scancode(keycode: u32) -> Option<ironrdp::input::Scancode> {
        // Map X11/GTK keycodes to RDP scancodes (Windows scancodes)
        // GTK uses X11 keycodes which are Linux evdev codes + 8
        // We need to convert to Windows scancode (Set 1)
        
        // Subtract 8 to get Linux evdev code
        let evdev = keycode.saturating_sub(8);
        
        // Map Linux evdev codes to Windows scancodes
        let scancode = match evdev {
            // Function keys
            1 => 0x01,   // ESC
            59 => 0x3B,  // F1
            60 => 0x3C,  // F2
            61 => 0x3D,  // F3
            62 => 0x3E,  // F4
            63 => 0x3F,  // F5
            64 => 0x40,  // F6
            65 => 0x41,  // F7
            66 => 0x42,  // F8
            67 => 0x43,  // F9
            68 => 0x44,  // F10
            87 => 0x57,  // F11
            88 => 0x58,  // F12
            
            // Number row
            41 => 0x29,  // ` ~
            2 => 0x02,   // 1 !
            3 => 0x03,   // 2 @
            4 => 0x04,   // 3 #
            5 => 0x05,   // 4 $
            6 => 0x06,   // 5 %
            7 => 0x07,   // 6 ^
            8 => 0x08,   // 7 &
            9 => 0x09,   // 8 *
            10 => 0x0A,  // 9 (
            11 => 0x0B,  // 0 )
            12 => 0x0C,  // - _
            13 => 0x0D,  // = +
            14 => 0x0E,  // Backspace
            
            // Top letter row
            15 => 0x0F,  // Tab
            16 => 0x10,  // Q
            17 => 0x11,  // W
            18 => 0x12,  // E
            19 => 0x13,  // R
            20 => 0x14,  // T
            21 => 0x15,  // Y
            22 => 0x16,  // U
            23 => 0x17,  // I
            24 => 0x18,  // O
            25 => 0x19,  // P
            26 => 0x1A,  // [ {
            27 => 0x1B,  // ] }
            28 => 0x1C,  // Enter
            
            // Middle letter row
            58 => 0x3A,  // Caps Lock
            30 => 0x1E,  // A
            31 => 0x1F,  // S
            32 => 0x20,  // D
            33 => 0x21,  // F
            34 => 0x22,  // G
            35 => 0x23,  // H
            36 => 0x24,  // J
            37 => 0x25,  // K
            38 => 0x26,  // L
            39 => 0x27,  // ; :
            40 => 0x28,  // ' "
            43 => 0x2B,  // \ |
            
            // Bottom letter row
            42 => 0x2A,  // Left Shift
            44 => 0x2C,  // Z
            45 => 0x2D,  // X
            46 => 0x2E,  // C
            47 => 0x2F,  // V
            48 => 0x30,  // B
            49 => 0x31,  // N
            50 => 0x32,  // M
            51 => 0x33,  // , <
            52 => 0x34,  // . >
            53 => 0x35,  // / ?
            54 => 0x36,  // Right Shift
            
            // Bottom row
            29 => 0x1D,  // Left Ctrl
            100 => 0xE038, // Right Alt (extended)
            56 => 0x38,  // Left Alt
            57 => 0x39,  // Space
            
            // Navigation cluster
            102 => 0xE047, // Home (extended)
            103 => 0xE048, // Up Arrow (extended)
            104 => 0xE049, // Page Up (extended)
            105 => 0xE04B, // Left Arrow (extended)
            106 => 0xE04D, // Right Arrow (extended)
            107 => 0xE04F, // End (extended)
            108 => 0xE050, // Down Arrow (extended)
            109 => 0xE051, // Page Down (extended)
            110 => 0xE052, // Insert (extended)
            111 => 0xE053, // Delete (extended)
            
            // Numpad
            69 => 0x45,  // Num Lock
            98 => 0x4A,  // Numpad /
            55 => 0x37,  // Numpad *
            74 => 0x4A,  // Numpad -
            71 => 0x47,  // Numpad 7
            72 => 0x48,  // Numpad 8
            73 => 0x49,  // Numpad 9
            78 => 0x4E,  // Numpad +
            75 => 0x4B,  // Numpad 4
            76 => 0x4C,  // Numpad 5
            77 => 0x4D,  // Numpad 6
            79 => 0x4F,  // Numpad 1
            80 => 0x50,  // Numpad 2
            81 => 0x51,  // Numpad 3
            82 => 0x52,  // Numpad 0
            83 => 0x53,  // Numpad .
            96 => 0xE01C, // Numpad Enter (extended)
            
            // Special keys
            119 => 0xE05F, // Pause/Break
            99 => 0xE037,  // Print Screen (extended)
            70 => 0x46,    // Scroll Lock
            127 => 0xE05D, // Menu/Application key (extended)
            
            // Windows/Super keys
            125 => 0xE05B, // Left Windows/Super (extended)
            126 => 0xE05C, // Right Windows/Super (extended)
            
            _ => {
                // Unknown key - return None to ignore
                return None;
            }
        };
        
        Some(ironrdp::input::Scancode::from_u16(scancode))
    }

    fn gtk_button_to_rdp_button(button: u32) -> Option<ironrdp::input::MouseButton> {
        match button {
            1 => Some(ironrdp::input::MouseButton::Left),
            2 => Some(ironrdp::input::MouseButton::Middle),
            3 => Some(ironrdp::input::MouseButton::Right),
            _ => None,
        }
    }

    fn send_fast_path_events(
        input_event_sender: &mpsc::UnboundedSender<RdpInputEvent>,
        input_events: smallvec::SmallVec<[ironrdp::pdu::input::fast_path::FastPathInputEvent; 2]>,
    ) {
        if !input_events.is_empty() {
            let _ = input_event_sender.send(RdpInputEvent::FastPath(input_events));
        }
    }

    fn update_image(&self, buffer: Vec<u32>, width: u16, height: u16) {
        // Convert once and cache: force alpha to 0xFF
        let cairo_buffer: Vec<u32> = buffer.iter().map(|&pixel| pixel | 0xFF000000).collect();
        
        *self.buffer.borrow_mut() = buffer;
        *self.cairo_buffer.borrow_mut() = cairo_buffer;
        *self.buffer_size.borrow_mut() = (width, height);
        self.drawing_area.queue_draw();
    }

    fn widget(&self) -> &gtk::DrawingArea {
        &self.drawing_area
    }
}

// Adapter for RDP event loop proxy to work with GTK's glib MainContext
struct RdpEventLoopProxy {
    sender: tokio::sync::mpsc::UnboundedSender<RdpOutputEvent>,
}

impl RdpEventLoopProxy {
    fn new(sender: tokio::sync::mpsc::UnboundedSender<RdpOutputEvent>) -> Self {
        Self { sender }
    }
}

impl rdp::RdpEventSender for RdpEventLoopProxy {
    fn send_event(&self, event: RdpOutputEvent) -> Result<(), ()> {
        self.sender.send(event).map_err(|_| ())
    }
}

fn create_remote_desktop_window(app: &Application, server: &str, username: &str, domain: &str, password: &str, main_window: &ApplicationWindow) {
    // Convert to owned strings to avoid lifetime issues
    let server = server.to_string();
    let username = username.to_string();
    let domain = domain.to_string();
    let password = password.to_string();
    
    // Hide the main window when opening remote desktop
    main_window.set_visible(false);
    
    // Create a new window for the remote desktop with normal decorations
    let rd_window = ApplicationWindow::builder()
        .application(app)
        .title(&format!("{} - Remote Desktop", server))
        .default_width(800)
        .default_height(600)
        .resizable(true)
        .decorated(true) // Keep normal window decorations
        .build();

    // Create RDP input/output channels
    let (input_event_sender, input_event_receiver) = RdpInputEvent::create_channel();
    let (output_event_sender, mut output_event_receiver) = tokio::sync::mpsc::unbounded_channel::<RdpOutputEvent>();

    // Create the RDP widget
    let rdp_widget = GtkRdpWidget::new(input_event_sender.clone());
    let rdp_widget = Rc::new(rdp_widget);

    // Create the top control bar (only visible in fullscreen/when pinned)
    let control_bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    // Set initial position estimate for centering (will be corrected later)
    // Assume ~250px control bar width and 800px window = center at ~275px
    control_bar.set_margin_start(275); 
    control_bar.set_margin_end(8);
    control_bar.set_margin_top(4);
    control_bar.set_margin_bottom(4);
    control_bar.add_css_class("osd"); // Overlay style
    control_bar.set_halign(gtk::Align::Start); // Always start from left, we'll position with margin
    control_bar.set_valign(gtk::Align::Start);
    
    // State for tracking control bar position in pixels
    let control_bar_x_position = std::rc::Rc::new(std::cell::RefCell::new(0.5)); // Start centered (0.5 = 50% of width)
    
    // Helper function for converting absolute position to relative position
    let absolute_to_relative = |absolute_pos: f64, window_width: f64, control_bar_width: f64| -> f64 {
        if window_width <= control_bar_width {
            0.5 // Default to center if window is too small
        } else {
            (absolute_pos / (window_width - control_bar_width)).clamp(0.0, 1.0)
        }
    };
    let user_has_moved_toolbar = std::rc::Rc::new(std::cell::RefCell::new(false));
    
    // Connection name label
    let connection_label = gtk::Label::new(Some(&format!("{} ({})", server, username)));
    connection_label.set_margin_start(8);
    connection_label.set_margin_end(8);
    connection_label.add_css_class("caption");
    
    // Pin button
    let pin_button = Button::new();
    pin_button.set_icon_name("view-pin-symbolic");
    pin_button.set_tooltip_text(Some("Pin controls"));
    pin_button.add_css_class("flat");
    pin_button.add_css_class("circular");
    
    // Menu button (fullscreen toggle)
    let menu_button = Button::new();
    menu_button.set_icon_name("view-fullscreen-symbolic");
    menu_button.set_tooltip_text(Some("Toggle fullscreen"));
    menu_button.add_css_class("flat");
    menu_button.add_css_class("circular");
    
    // Close button
    let close_button = Button::new();
    close_button.set_icon_name("window-close-symbolic");
    close_button.set_tooltip_text(Some("Disconnect"));
    close_button.add_css_class("flat");
    close_button.add_css_class("circular");
    close_button.add_css_class("destructive-action");
    
    // Pack control bar with new order: connection_label, pin, fullscreen, close
    control_bar.append(&connection_label);
    control_bar.append(&pin_button);
    control_bar.append(&menu_button);
    control_bar.append(&close_button);
    
    // Create overlay to layer control bar over drawing area
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(rdp_widget.widget()));
    overlay.add_overlay(&control_bar);

        // Setup RDP output event handling
    let rdp_widget_events = rdp_widget.clone();
    let rd_window_events = rd_window.clone();
    
    // Bridge tokio channel to GTK main thread
    glib::spawn_future_local(async move {
        while let Some(event) = output_event_receiver.recv().await {
            match event {
                RdpOutputEvent::Image { buffer, width, height } => {
                    rdp_widget_events.update_image(buffer, width.get(), height.get());
                }
                RdpOutputEvent::ConnectionFailure(error) => {
                    eprintln!("RDP Connection failed: {:?}", error);
                    
                    // Get user-friendly error message
                    let (title, message) = format_rdp_error(&error);
                    
                    // Show error dialog and close window when user dismisses it
                    let dialog = gtk::AlertDialog::builder()
                        .message(&title)
                        .detail(&message)
                        .build();
                    
                    let window_to_close = rd_window_events.clone();
                    dialog.choose(Some(&rd_window_events), None::<&gtk::gio::Cancellable>, move |_result| {
                        // Close window after user dismisses the error dialog
                        window_to_close.close();
                    });
                }
                RdpOutputEvent::Terminated(result) => {
                    match result {
                        Ok(reason) => println!("RDP session terminated: {:?}", reason),
                        Err(error) => eprintln!("RDP session error: {:?}", error),
                    }
                    rd_window_events.close();
                }
                RdpOutputEvent::PointerDefault => {
                    // TODO: Handle default pointer
                }
                RdpOutputEvent::PointerHidden => {
                    // TODO: Handle hidden pointer
                }
                RdpOutputEvent::PointerPosition { x, y } => {
                    // TODO: Handle pointer position
                }
                RdpOutputEvent::PointerBitmap(_pointer) => {
                    // TODO: Handle custom pointer bitmap
                }
            }
        }
    });

    // Set up fullscreen toggle key (F11)
    let rd_window_f11 = rd_window.clone();

    // Setup close button handler
    let input_sender_close = input_event_sender.clone();
    close_button.connect_clicked(move |_| {
        let _ = input_sender_close.send(RdpInputEvent::Close);
    });
    
    // Add drag functionality using the overlay for consistent coordinates
    let gesture_click = gtk::GestureClick::new();
    gesture_click.set_button(1); // Left mouse button
    
    let is_dragging = std::rc::Rc::new(std::cell::RefCell::new(false));
    let drag_start_x = std::rc::Rc::new(std::cell::RefCell::new(0.0));
    let drag_start_position = std::rc::Rc::new(std::cell::RefCell::new(0.0));
    let drag_offset_x = std::rc::Rc::new(std::cell::RefCell::new(0.0)); // Offset within control bar
    
    // Handle mouse press to start drag
    let is_dragging_press = is_dragging.clone();
    let drag_start_x_press = drag_start_x.clone();
    let drag_start_position_press = drag_start_position.clone();
    let drag_offset_x_press = drag_offset_x.clone();
    let control_bar_x_position_press = control_bar_x_position.clone();
    let control_bar_for_press = control_bar.clone();
    
    gesture_click.connect_pressed(move |_, _, x, y| {
        // Check if click is within the control bar area using bounds
        // Extend the draggable area to include padding above the control bar
        let control_bar_x = control_bar_for_press.margin_start() as f64;
        let control_bar_y = 0.0; // Start draggable area from top of window instead of margin_top
        let control_bar_width = control_bar_for_press.width() as f64;
        let control_bar_height = control_bar_for_press.height() as f64 + control_bar_for_press.margin_top() as f64; // Include the top margin in draggable height
        
        if x >= control_bar_x && x <= control_bar_x + control_bar_width &&
           y >= control_bar_y && y <= control_bar_y + control_bar_height {
            *is_dragging_press.borrow_mut() = true;
            *drag_start_x_press.borrow_mut() = x;
            *drag_start_position_press.borrow_mut() = *control_bar_x_position_press.borrow();
            *drag_offset_x_press.borrow_mut() = x - control_bar_x; // Offset within control bar
        }
    });
    
    // Handle mouse release to end drag
    let is_dragging_release = is_dragging.clone();
    
    gesture_click.connect_released(move |_, _, _, _| {
        *is_dragging_release.borrow_mut() = false;
    });
    
    // Handle cancellation
    let is_dragging_cancel = is_dragging.clone();
    
    gesture_click.connect_cancel(move |_, _| {
        *is_dragging_cancel.borrow_mut() = false;
    });
    
    // Add the gesture to the overlay for consistent coordinate system
    overlay.add_controller(gesture_click);
    
    // Add motion tracking for real-time drag feedback
    let motion_controller = gtk::EventControllerMotion::new();
    let is_dragging_motion = is_dragging.clone();
    let drag_offset_x_motion = drag_offset_x.clone();
    let control_bar_for_motion = control_bar.clone();
    let control_bar_x_position_motion = control_bar_x_position.clone();
    let user_has_moved_toolbar_motion = user_has_moved_toolbar.clone();
    let overlay_for_motion = overlay.clone();
    
    motion_controller.connect_motion(move |controller, x, _| {
        // Only process drag if we're flagged as dragging AND there's an actual button press
        let is_currently_dragging = *is_dragging_motion.borrow();
        
        if is_currently_dragging {
            // Check if we have a current event that includes button state
            if let Some(event) = controller.current_event() {
                // Check if this is a motion event with no button pressed
                if let Some(_) = event.downcast_ref::<gtk::gdk::ButtonEvent>() {
                    // This is actually a button event, not motion
                    return;
                }
                
                // For motion events, check modifier state for button press
                let state = event.modifier_state();
                // GDK_BUTTON1_MASK indicates left mouse button is pressed
                if !state.contains(gtk::gdk::ModifierType::BUTTON1_MASK) {
                    // No button pressed - spurious motion event, ignore it
                    return;
                }
            }
            
            let offset_within_bar = *drag_offset_x_motion.borrow();
            
            // Calculate the new control bar position
            // x is the current mouse position, we want the control bar to be positioned
            // so that the mouse is still at the same offset within the control bar
            let new_control_bar_x = x - offset_within_bar;
            
            // Get window width for boundary checking
            let window_width = overlay_for_motion.width() as f64;
            let control_bar_width = control_bar_for_motion.width() as f64;
            
            // Only apply boundaries if we have valid dimensions
            if window_width > 0.0 && control_bar_width > 0.0 {
                // Clamp position to stay within window bounds
                let clamped_x = new_control_bar_x.max(0.0).min(window_width - control_bar_width);
                
                // Convert absolute position to relative position (0.0 to 1.0)
                let relative_pos = absolute_to_relative(clamped_x, window_width, control_bar_width);
                
                // Check if position actually changed (with small tolerance for floating point precision)
                let current_relative_position = *control_bar_x_position_motion.borrow();
                let position_changed = (relative_pos - current_relative_position).abs() > 0.01;
                
                // Update position in real-time during drag with exact precision
                let margin_start = clamped_x.round() as i32;
                control_bar_for_motion.set_margin_start(margin_start);
                *control_bar_x_position_motion.borrow_mut() = relative_pos;
                
                // Only mark as user-moved if position actually changed
                if position_changed {
                    *user_has_moved_toolbar_motion.borrow_mut() = true;
                }
            }
        }
    });
    
    // Handle cursor leaving the window during drag
    let is_dragging_leave = is_dragging.clone();
    motion_controller.connect_leave(move |_| {
        // Stop dragging when cursor leaves the window
        *is_dragging_leave.borrow_mut() = false;
        // Note: Visibility will be handled by the show/hide motion controller
    });
    
    overlay.add_controller(motion_controller);
    
    // Add a move cursor when hovering over the draggable area (extended control bar area)
    let motion_controller_cursor = gtk::EventControllerMotion::new();
    let control_bar_for_cursor = control_bar.clone();
    
    motion_controller_cursor.connect_motion(move |controller, x, y| {
        // Check if cursor is in the extended draggable area
        let control_bar_x = control_bar_for_cursor.margin_start() as f64;
        let control_bar_y = 0.0; // Start from top of window
        let control_bar_width = control_bar_for_cursor.width() as f64;
        let control_bar_height = control_bar_for_cursor.height() as f64 + control_bar_for_cursor.margin_top() as f64; // Include top margin
        
        if x >= control_bar_x && x <= control_bar_x + control_bar_width &&
           y >= control_bar_y && y <= control_bar_y + control_bar_height {
            if let Some(widget) = controller.widget() {
                widget.set_cursor_from_name(Some("grab"));
            }
        } else {
            if let Some(widget) = controller.widget() {
                widget.set_cursor_from_name(Some("default"));
            }
        }
    });
    
    motion_controller_cursor.connect_leave(move |controller| {
        if let Some(widget) = controller.widget() {
            widget.set_cursor_from_name(Some("default"));
        }
    });
    
    overlay.add_controller(motion_controller_cursor);
    
    // Initially hide control bar until it's been centered to avoid showing it at wrong position
    control_bar.set_visible(false);
    
    // State management for control bar visibility
    let is_pinned = std::rc::Rc::new(std::cell::RefCell::new(false));
    let is_fullscreen = std::rc::Rc::new(std::cell::RefCell::new(false));
    
    // Pin button functionality
    let control_bar_for_pin = control_bar.clone();
    let is_pinned_clone = is_pinned.clone();
    let is_fullscreen_for_pin = is_fullscreen.clone();
    pin_button.connect_clicked(move |button| {
        let mut pinned = is_pinned_clone.borrow_mut();
        *pinned = !*pinned;
        
        if *pinned {
            // Add pressed/active state styling
            button.add_css_class("suggested-action");
            button.set_tooltip_text(Some("Unpin controls"));
            control_bar_for_pin.set_visible(true);
        } else {
            // Remove pressed/active state styling
            button.remove_css_class("suggested-action");
            button.set_tooltip_text(Some("Pin controls"));
            // In windowed mode, always show controls
            // In fullscreen mode, hide unless mouse is at top
            if *is_fullscreen_for_pin.borrow() {
                control_bar_for_pin.set_visible(false);
            }
        }
    });
    
    // Close button functionality
    let rd_window_for_close = rd_window.clone();
    let main_window_for_close_button = main_window.clone();
    close_button.connect_clicked(move |_| {
        // Show the main window again before closing
        main_window_for_close_button.set_visible(true);
        main_window_for_close_button.present();
        rd_window_for_close.close();
    });
    
    // Toggle fullscreen button
    let rd_window_for_menu = rd_window.clone();
    let is_fullscreen_for_menu = is_fullscreen.clone();
    let menu_button_for_toggle = menu_button.clone();
    menu_button.connect_clicked(move |_| {
        let fullscreen = is_fullscreen_for_menu.borrow();
        if *fullscreen {
            rd_window_for_menu.unfullscreen();
            menu_button_for_toggle.set_icon_name("view-fullscreen-symbolic");
            menu_button_for_toggle.set_tooltip_text(Some("Enter fullscreen"));
        } else {
            rd_window_for_menu.fullscreen();
            menu_button_for_toggle.set_icon_name("view-restore-symbolic");
            menu_button_for_toggle.set_tooltip_text(Some("Exit fullscreen"));
        }
    });
    
    // Mouse motion to show/hide controls (only in fullscreen when not pinned)
    let motion_controller_show_hide = gtk::EventControllerMotion::new();
    let control_bar_for_motion_show_hide = control_bar.clone();
    let is_pinned_for_motion = is_pinned.clone();
    let is_fullscreen_for_motion = is_fullscreen.clone();
    let is_dragging_for_motion = is_dragging.clone(); // Add dragging state check
    
    motion_controller_show_hide.connect_motion(move |_, _x, y| {
        let pinned = *is_pinned_for_motion.borrow();
        let fullscreen = *is_fullscreen_for_motion.borrow();
        let dragging = *is_dragging_for_motion.borrow();
        
        // In windowed mode or when pinned, always show controls
        // In fullscreen mode when not pinned, show only when mouse is near top OR when dragging
        if !fullscreen || pinned || dragging || (fullscreen && y < 50.0) {
            control_bar_for_motion_show_hide.set_visible(true);
        } else if fullscreen && !pinned && !dragging && y >= 50.0 {
            control_bar_for_motion_show_hide.set_visible(false);
        }
    });
    overlay.add_controller(motion_controller_show_hide);
    
    // Key controller for fullscreen toggle and escape
    let key_controller = gtk::EventControllerKey::new();
    let rd_window_for_key = rd_window.clone();
    let is_fullscreen_for_key = is_fullscreen.clone();
    let control_bar_for_key = control_bar.clone();
    
    key_controller.connect_key_pressed(move |_, key, _, _| {
        match key {
            gtk::gdk::Key::F11 => {
                let mut fullscreen = is_fullscreen_for_key.borrow_mut();
                *fullscreen = !*fullscreen;
                
                if *fullscreen {
                    rd_window_for_key.fullscreen();
                } else {
                    rd_window_for_key.unfullscreen();
                    // Always show controls in windowed mode
                    control_bar_for_key.set_visible(true);
                }
                gtk::glib::Propagation::Stop
            },
            gtk::gdk::Key::Escape => {
                let mut fullscreen = is_fullscreen_for_key.borrow_mut();
                if *fullscreen {
                    rd_window_for_key.unfullscreen();
                    *fullscreen = false;
                    // Always show controls in windowed mode
                    control_bar_for_key.set_visible(true);
                }
                gtk::glib::Propagation::Stop
            },
            _ => gtk::glib::Propagation::Proceed
        }
    });
    rd_window.add_controller(key_controller);
    
    // Window state tracking for fullscreen changes and resizing
    let is_fullscreen_for_state = is_fullscreen.clone();
    let control_bar_for_state = control_bar.clone();
    let control_bar_x_position_for_state = control_bar_x_position.clone();
    let user_has_moved_toolbar_for_state = user_has_moved_toolbar.clone();
    
    rd_window.connect_notify_local(Some("fullscreened"), move |window, _| {
        let mut fullscreen = is_fullscreen_for_state.borrow_mut();
        let new_fullscreen = window.is_fullscreen();
        
        if new_fullscreen != *fullscreen {
            *fullscreen = new_fullscreen;
            
            // Re-center toolbar on both entering and exiting fullscreen (if user hasn't moved it)
            if !*user_has_moved_toolbar_for_state.borrow() {
                // Use idle callback for immediate centering after fullscreen transition
                gtk::glib::idle_add_local_once({
                    let control_bar_for_center = control_bar_for_state.clone();
                    let control_bar_x_position_for_center = control_bar_x_position_for_state.clone();
                    let window_for_center = window.clone();
                    move || {
                        let window_width = window_for_center.width() as f64;
                        let control_bar_width = control_bar_for_center.width() as f64;
                        if window_width > 0.0 && control_bar_width > 0.0 {
                            // If user hasn't moved toolbar, keep it centered (0.5)
                            let relative_pos = 0.5;
                            
                            // Convert relative position to absolute for this window size
                            let center_x = if window_width <= control_bar_width {
                                0.0
                            } else {
                                relative_pos * (window_width - control_bar_width)
                            };
                            
                            *control_bar_x_position_for_center.borrow_mut() = relative_pos;
                            control_bar_for_center.set_margin_start(center_x as i32);
                        }
                    }
                });
            } else {
                // User has moved toolbar, maintain relative position
                gtk::glib::idle_add_local_once({
                    let control_bar_for_maintain = control_bar_for_state.clone();
                    let control_bar_x_position_for_maintain = control_bar_x_position_for_state.clone();
                    let window_for_maintain = window.clone();
                    move || {
                        let window_width = window_for_maintain.width() as f64;
                        let control_bar_width = control_bar_for_maintain.width() as f64;
                        if window_width > 0.0 && control_bar_width > 0.0 {
                            let current_relative_position = *control_bar_x_position_for_maintain.borrow();
                            
                            // Convert relative position to absolute for this window size
                            let absolute_x = if window_width <= control_bar_width {
                                0.0
                            } else {
                                current_relative_position * (window_width - control_bar_width)
                            };
                            
                            control_bar_for_maintain.set_margin_start(absolute_x as i32);
                        }
                    }
                });
            }
            
            if !*fullscreen {
                // Exiting fullscreen - always show controls in windowed mode
                control_bar_for_state.set_visible(true);
            }
        }
    });
    
    // Handle maximize state changes to re-center toolbar if not moved by user
    let control_bar_for_maximize = control_bar.clone();
    let control_bar_x_position_for_maximize = control_bar_x_position.clone();
    let user_has_moved_toolbar_for_maximize = user_has_moved_toolbar.clone();
    let overlay_for_maximize = overlay.clone();
    
    rd_window.connect_notify_local(Some("maximized"), move |_, _| {
        // Re-center if user hasn't moved the toolbar
        if !*user_has_moved_toolbar_for_maximize.borrow() {
            gtk::glib::idle_add_local_once({
                let control_bar_for_center_max = control_bar_for_maximize.clone();
                let control_bar_x_position_for_center_max = control_bar_x_position_for_maximize.clone();
                let overlay_for_center_max = overlay_for_maximize.clone();
                move || {
                    let window_width = overlay_for_center_max.width() as f64;
                    let control_bar_width = control_bar_for_center_max.width() as f64;
                    if window_width > 0.0 && control_bar_width > 0.0 {
                        // If user hasn't moved toolbar, keep it centered (0.5)
                        let relative_pos = 0.5;
                        
                        // Convert relative position to absolute for this window size
                        let center_x = if window_width <= control_bar_width {
                            0.0
                        } else {
                            relative_pos * (window_width - control_bar_width)
                        };
                        
                        *control_bar_x_position_for_center_max.borrow_mut() = relative_pos;
                        control_bar_for_center_max.set_margin_start(center_x as i32);
                    }
                }
            });
        }
    });
    
    // Handle window resize to keep toolbar in view and re-center if needed
    let control_bar_for_resize = control_bar.clone();
    let control_bar_x_position_for_resize = control_bar_x_position.clone();
    let user_has_moved_toolbar_for_resize = user_has_moved_toolbar.clone();
    let overlay_for_resize = overlay.clone();
    
    // Add a size allocation handler to handle resizing
    rd_window.connect_notify_local(Some("default-width"), move |_, _| {
        gtk::glib::idle_add_local_once({
            let control_bar_for_resize_inner = control_bar_for_resize.clone();
            let control_bar_x_position_for_resize_inner = control_bar_x_position_for_resize.clone();
            let user_has_moved_toolbar_for_resize_inner = user_has_moved_toolbar_for_resize.clone();
            let overlay_for_resize_inner = overlay_for_resize.clone();
            
            move || {
                let window_width = overlay_for_resize_inner.width() as f64;
                let control_bar_width = control_bar_for_resize_inner.width() as f64;
                let current_relative_position = *control_bar_x_position_for_resize_inner.borrow();
                
                if window_width > 0.0 && control_bar_width > 0.0 {
                    if !*user_has_moved_toolbar_for_resize_inner.borrow() {
                        // User hasn't moved it, so re-center (0.5 relative position)
                        let relative_pos = 0.5;
                        let center_x = if window_width <= control_bar_width {
                            0.0
                        } else {
                            relative_pos * (window_width - control_bar_width)
                        };
                        *control_bar_x_position_for_resize_inner.borrow_mut() = relative_pos;
                        control_bar_for_resize_inner.set_margin_start(center_x as i32);
                    } else {
                        // User has moved it, maintain relative position
                        let absolute_x = if window_width <= control_bar_width {
                            0.0
                        } else {
                            current_relative_position * (window_width - control_bar_width)
                        };
                        
                        // Clamp to ensure it's still within bounds
                        let max_position = (window_width - control_bar_width).max(0.0);
                        let clamped_x = absolute_x.clamp(0.0, max_position);
                        
                        // Update relative position in case we had to clamp
                        let new_relative_pos = if window_width <= control_bar_width {
                            0.5
                        } else {
                            (clamped_x / (window_width - control_bar_width)).clamp(0.0, 1.0)
                        };
                        
                        *control_bar_x_position_for_resize_inner.borrow_mut() = new_relative_pos;
                        control_bar_for_resize_inner.set_margin_start(clamped_x as i32);
                    }
                }
            }
        });
    });

    rd_window.set_child(Some(&overlay));

    // Handle window close
    let main_window_for_close = main_window.clone();
    let input_sender_window_close = input_event_sender.clone();
    rd_window.connect_close_request(move |_| {
        // Send close event to RDP client
        let _ = input_sender_window_close.send(RdpInputEvent::Close);
        // Show the main window again when remote desktop closes
        main_window_for_close.set_visible(true);
        main_window_for_close.present();
        gtk::glib::Propagation::Proceed
    });

    // Create and start RDP client
    let config = create_rdp_config(&server, &username, &domain, &password);

    // Clone the output sender for the RDP client
    let output_sender_for_client = output_event_sender.clone();
    
    // Clone input sender for resize events before moving into thread
    let input_sender_resize = input_event_sender.clone();
    
    // Start RDP client in a separate thread with Tokio runtime
    std::thread::spawn(move || {
        let dvc_pipe_proxy_factory = DvcPipeProxyFactory::new(input_event_sender.clone());
        
        let rdp_client = RdpClient {
            config,
            event_loop_proxy: RdpEventLoopProxy::new(output_sender_for_client),
            input_event_receiver,
            cliprdr_factory: None, // Can be extended for clipboard support
            dvc_pipe_proxy_factory,
        };

        // Create Tokio runtime and run the RDP client
        let runtime = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
        runtime.block_on(async move {
            rdp_client.run().await;
        });
    });

    // Show window and focus RDP widget
    rd_window.present();
    rdp_widget.widget().grab_focus();
    
    // Handle window resize to request new desktop size from RDP server
    let last_resize_time = Rc::new(RefCell::new(std::time::Instant::now()));
    let resize_pending = Rc::new(RefCell::new(false));
    
    rd_window.connect_default_width_notify({
        let rd_window = rd_window.clone();
        let last_resize_time = last_resize_time.clone();
        let resize_pending = resize_pending.clone();
        let input_sender = input_sender_resize.clone();
        
        move |_| {
            // Debounce resize events - only send after 500ms of no resize activity
            *last_resize_time.borrow_mut() = std::time::Instant::now();
            
            if !*resize_pending.borrow() {
                *resize_pending.borrow_mut() = true;
                
                let last_resize_time = last_resize_time.clone();
                let resize_pending = resize_pending.clone();
                let input_sender = input_sender.clone();
                let rd_window = rd_window.clone();
                
                gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
                    let elapsed = last_resize_time.borrow().elapsed();
                    
                    if elapsed >= std::time::Duration::from_millis(500) {
                        // Send resize event to RDP server
                        let width = rd_window.default_width() as u16;
                        let height = rd_window.default_height() as u16;
                        
                        let _ = input_sender.send(RdpInputEvent::Resize {
                            width,
                            height,
                            scale_factor: 100, // Default scale factor
                            physical_size: None,
                        });
                        
                        *resize_pending.borrow_mut() = false;
                    }
                });
            }
        }
    });
    
    rd_window.connect_default_height_notify({
        let rd_window = rd_window.clone();
        let last_resize_time = last_resize_time.clone();
        let resize_pending = resize_pending.clone();
        let input_sender = input_sender_resize.clone();
        
        move |_| {
            // Debounce resize events
            *last_resize_time.borrow_mut() = std::time::Instant::now();
            
            if !*resize_pending.borrow() {
                *resize_pending.borrow_mut() = true;
                
                let last_resize_time = last_resize_time.clone();
                let resize_pending = resize_pending.clone();
                let input_sender = input_sender.clone();
                let rd_window = rd_window.clone();
                
                gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
                    let elapsed = last_resize_time.borrow().elapsed();
                    
                    if elapsed >= std::time::Duration::from_millis(500) {
                        let width = rd_window.default_width() as u16;
                        let height = rd_window.default_height() as u16;
                        
                        let _ = input_sender.send(RdpInputEvent::Resize {
                            width,
                            height,
                            scale_factor: 100,
                            physical_size: None,
                        });
                        
                        *resize_pending.borrow_mut() = false;
                    }
                });
            }
        }
    });
    
    // Center the control bar after the window is fully presented and laid out
    let control_bar_for_timeout_center = control_bar.clone();
    let control_bar_x_position_for_timeout_center = control_bar_x_position.clone();
    let overlay_for_timeout_center = overlay.clone();
    
    gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
        // Force allocation to ensure we have proper dimensions
        control_bar_for_timeout_center.queue_allocate();
        overlay_for_timeout_center.queue_allocate();
        
        // Give it one more idle cycle to ensure allocation
        gtk::glib::idle_add_local_once({
            let control_bar_clone = control_bar_for_timeout_center.clone();
            let control_bar_x_position_clone = control_bar_x_position_for_timeout_center.clone();
            let overlay_clone = overlay_for_timeout_center.clone();
            
            move || {
                let window_width = overlay_clone.width() as f64;
                let control_bar_width = control_bar_clone.width() as f64;
                
                if window_width > 0.0 && control_bar_width > 0.0 {
                    // Set initial position to center (0.5 relative position)
                    let relative_pos = 0.5;
                    let center_x = if window_width <= control_bar_width {
                        0.0
                    } else {
                        relative_pos * (window_width - control_bar_width)
                    };
                    *control_bar_x_position_clone.borrow_mut() = relative_pos;
                    control_bar_clone.set_margin_start(center_x as i32);
                }
                
                // Show only after positioning
                control_bar_clone.set_visible(true);
            }
        });
    });
}

fn main() -> glib::ExitCode {
    // Initialize tracing subscriber for debug logging
    // Set RUST_LOG environment variable to control log level, e.g.:
    // RUST_LOG=debug ./irontsc
    // RUST_LOG=ironrdp=trace ./irontsc
    // RUST_LOG=ironrdp=debug,ironrdp_connector=trace ./irontsc
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
        )
        .with_target(true)
        .with_line_number(true)
        .init();

    // Create a new application
    let app = Application::builder().application_id(APP_ID).build();

     // Connect to "activate" signal of `app`
    app.connect_activate(build_ui);

    // Run the application
    app.run()
}

fn build_ui(app: &Application) {
    // Add server text box
    let server = Text::new();

    // Create a window and set the title
    let window = ApplicationWindow::builder()
        .application(app)
        .title("Remote Desktop Connection")
        .child(&server)
        .build();

    let window_clone = window.clone();

    // Create UI elements
    let dialog_box = gtk::Box::new(gtk::Orientation::Vertical, 5);
    dialog_box.set_margin_top(12);
    dialog_box.set_margin_bottom(12);
    dialog_box.set_margin_start(12);
    dialog_box.set_margin_end(12);
    
    // Add heading
    let heading = gtk::Label::new(Some("Remote Desktop Connection"));
    heading.add_css_class("title-1");
    dialog_box.append(&heading);

    // Load saved settings
    let config = AppConfig::load();
    
    let server_input = gtk::Entry::new();
    server_input.set_hexpand(true);
    server_input.set_text(&config.server);
    let server_box = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    let connect_label = gtk::Label::new(Some("Computer"));
    connect_label.set_width_chars(10);
    connect_label.set_xalign(0.0);
    server_box.append(&connect_label);
    server_box.append(&server_input);

    let username_input = gtk::Entry::new();
    username_input.set_hexpand(true);
    username_input.set_text(&config.username);
    let username_box = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    let username_label = gtk::Label::new(Some("Username"));
    username_label.set_width_chars(10);
    username_label.set_xalign(0.0);
    username_box.append(&username_label);
    username_box.append(&username_input);

    let domain_input = gtk::Entry::new();
    domain_input.set_hexpand(true);
    domain_input.set_text(&config.domain);
    let domain_box = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    let domain_label = gtk::Label::new(Some("Domain"));
    domain_label.set_width_chars(10);
    domain_label.set_xalign(0.0);
    domain_box.append(&domain_label);
    domain_box.append(&domain_input);

    let password_input = gtk::Entry::new();
    password_input.set_visibility(false);
    password_input.set_hexpand(true);
    let password_box = gtk::Box::new(gtk::Orientation::Horizontal, 5);
    let password_label = gtk::Label::new(Some("Password"));
    password_label.set_width_chars(10);
    password_label.set_xalign(0.0);
    password_box.append(&password_label);
    password_box.append(&password_input);

    let button = Button::with_label("Connect");
    let server_input_clone = server_input.clone();
    let username_input_clone = username_input.clone();
    let domain_input_clone = domain_input.clone();
    let password_input_clone = password_input.clone();
    let window_for_dialog = window_clone.clone();
    let app_clone = app.clone();
    let button_clone = button.clone();
    
    // State for tracking connection
    let is_connecting = std::rc::Rc::new(std::cell::RefCell::new(false));
    let connection_timeout_id: std::rc::Rc<std::cell::RefCell<Option<gtk::glib::SourceId>>> = std::rc::Rc::new(std::cell::RefCell::new(None));
    let connecting_window_ref: std::rc::Rc<std::cell::RefCell<Option<ApplicationWindow>>> = std::rc::Rc::new(std::cell::RefCell::new(None));
    
    button.connect_clicked(move |_| {
        let mut connecting = is_connecting.borrow_mut();
        
        if *connecting {
            // Currently connecting - cancel the connection
            if let Some(timeout_id) = connection_timeout_id.borrow_mut().take() {
                timeout_id.remove();
            }
            
            // Close connecting window if it exists
            if let Some(window) = connecting_window_ref.borrow_mut().take() {
                window.close();
            }
            
            *connecting = false;
            button_clone.set_label("Connect");
            return;
        }
        
        let server_text = server_input_clone.buffer().text();
        let username_text = username_input_clone.buffer().text();
        let domain_text = domain_input_clone.buffer().text();
        let password_text = password_input_clone.buffer().text();
        
        if server_text.is_empty() || username_text.is_empty() {
            let dialog = gtk::AlertDialog::builder()
                .message("Missing Information")
                .detail("Please enter both Computer and Username")
                .build();
            dialog.show(Some(&window_for_dialog));
        } else {
            // Start connecting
            *connecting = true;
            button_clone.set_label("Cancel");
            
            // Save settings
            let config = AppConfig {
                server: server_text.to_string(),
                username: username_text.to_string(),
                domain: domain_text.to_string(),
            };
            config.save();
            
            // Create a small connection status window
            let connecting_window = ApplicationWindow::builder()
                .application(&app_clone)
                .title("Connecting")
                .width_request(300)
                .height_request(120)
                .resizable(false)
                .build();
            
            let connecting_box = gtk::Box::new(gtk::Orientation::Vertical, 10);
            connecting_box.set_margin_top(20);
            connecting_box.set_margin_bottom(20);
            connecting_box.set_margin_start(20);
            connecting_box.set_margin_end(20);
            connecting_box.set_halign(gtk::Align::Center);
            connecting_box.set_valign(gtk::Align::Center);
            
            let connecting_label = gtk::Label::new(Some("Connecting..."));
            connecting_label.add_css_class("title-4");
            
            let detail_label = gtk::Label::new(Some(&format!("Opening remote desktop connection to {}", server_text)));
            detail_label.set_wrap(true);
            detail_label.set_justify(gtk::Justification::Center);
            
            connecting_box.append(&connecting_label);
            connecting_box.append(&detail_label);
            connecting_window.set_child(Some(&connecting_box));
            
            // Show the connecting window
            connecting_window.present();
            
            // Store window reference for potential cancellation
            *connecting_window_ref.borrow_mut() = Some(connecting_window.clone());
            
            let app_for_connection = app_clone.clone();
            let server_for_connection = server_text.to_string();
            let username_for_connection = username_text.to_string();
            let domain_for_connection = domain_text.to_string();
            let password_for_connection = password_text.to_string();
            let main_window_for_connection = window_for_dialog.clone();
            let button_for_reset = button_clone.clone();
            let is_connecting_for_timeout = is_connecting.clone();
            let connecting_window_ref_for_timeout = connecting_window_ref.clone();
            
            // Auto-close connecting window and create remote desktop window after a short delay
            let timeout_id = gtk::glib::timeout_add_local_once(std::time::Duration::from_millis(1000), move || {
                // Close the connecting window
                connecting_window.close();
                
                // Clear window reference
                *connecting_window_ref_for_timeout.borrow_mut() = None;
                
                // Reset connecting state
                *is_connecting_for_timeout.borrow_mut() = false;
                button_for_reset.set_label("Connect");

                // Debug output
                let domain_display = if domain_for_connection.is_empty() { 
                    "(no domain)".to_string() 
                } else { 
                    domain_for_connection.clone() 
                };
                let password_display = if !password_for_connection.is_empty() {
                    format!("password: {} chars", password_for_connection.len())
                } else {
                    "NO PASSWORD".to_string()
                };
                println!("Connecting to {} - Username: '{}', Domain: '{}', {}", 
                    server_for_connection, username_for_connection, domain_display, password_display);
                
                // Create the remote desktop window and pass the main window
                create_remote_desktop_window(&app_for_connection, &server_for_connection, &username_for_connection, &domain_for_connection, &password_for_connection, &main_window_for_connection);
            });
            
            *connection_timeout_id.borrow_mut() = Some(timeout_id);
        }
    });
    
    // Set button as default and make entries activate it on Enter
    button.add_css_class("suggested-action");
    server_input.set_activates_default(true);
    username_input.set_activates_default(true);
    domain_input.set_activates_default(true);
    password_input.set_activates_default(true);
    
    dialog_box.append(&server_box);
    dialog_box.append(&username_box);
    dialog_box.append(&domain_box);
    dialog_box.append(&password_box);
    dialog_box.append(&button);
    window_clone.set_child(Some(&dialog_box));
    window_clone.set_default_widget(Some(&button));

    // Present window
    window.present();
    
    // Clear text selection after window is shown
    let server_input_deselect = server_input.clone();
    let username_input_deselect = username_input.clone();
    let domain_input_deselect = domain_input.clone();
    let password_input_deselect = password_input.clone();
    
    glib::idle_add_local_once(move || {
        // Move cursor to end and clear selection
        server_input_deselect.set_position(-1);
        username_input_deselect.set_position(-1);
        domain_input_deselect.set_position(-1);
        password_input_deselect.set_position(-1);
    });
}