use gtk::{prelude::*, ApplicationWindow, Button, Text};
use gtk::{glib, Application};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const APP_ID: &str = "org.gtk_rs.IronTsc";
const CONFIG_FILE: &str = "config.json";

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

fn create_remote_desktop_window(app: &Application, server: &str, username: &str, _domain: &str, main_window: &ApplicationWindow) {
    // Convert to owned strings to avoid lifetime issues
    let server = server.to_string();
    let username = username.to_string();
    
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

    // Create the main drawing area for remote desktop content
    let drawing_area = gtk::DrawingArea::new();
    drawing_area.set_hexpand(true);
    drawing_area.set_vexpand(true);
    drawing_area.set_can_focus(true);
    drawing_area.set_focusable(true);

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
    overlay.set_child(Some(&drawing_area));
    overlay.add_overlay(&control_bar);
    
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
            button.set_icon_name("view-pin-filled-symbolic");
            button.set_tooltip_text(Some("Unpin controls"));
            control_bar_for_pin.set_visible(true);
        } else {
            button.set_icon_name("view-pin-symbolic");
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

    // Set up drawing area content
    let server_for_draw = server.clone();
    let username_for_draw = username.clone();
    drawing_area.set_draw_func(move |_, cr, width, height| {
        // Dark desktop background
        cr.set_source_rgb(0.05, 0.05, 0.1);
        cr.rectangle(0.0, 0.0, width as f64, height as f64);
        let _ = cr.fill();

        // Subtle grid pattern
        cr.set_source_rgb(0.1, 0.1, 0.15);
        cr.set_line_width(1.0);
        
        let grid_size = 50.0;
        for i in 0..((width as f64 / grid_size) as i32) {
            let x = i as f64 * grid_size;
            cr.move_to(x, 0.0);
            cr.line_to(x, height as f64);
            let _ = cr.stroke();
        }
        
        for i in 0..((height as f64 / grid_size) as i32) {
            let y = i as f64 * grid_size;
            cr.move_to(0.0, y);
            cr.line_to(width as f64, y);
            let _ = cr.stroke();
        }

        // Centered connection info
        cr.set_source_rgb(0.6, 0.6, 0.7);
        let main_text = &format!("Connected to {}", server_for_draw);
        let text_extents = cr.text_extents(main_text).unwrap();
        let x = (width as f64 - text_extents.width()) / 2.0;
        let y = (height as f64) / 2.0;
        cr.move_to(x, y);
        let _ = cr.show_text(main_text);

        cr.set_source_rgb(0.4, 0.4, 0.5);
        let user_text = &format!("User: {}", username_for_draw);
        let user_extents = cr.text_extents(user_text).unwrap();
        let user_x = (width as f64 - user_extents.width()) / 2.0;
        cr.move_to(user_x, y + 25.0);
        let _ = cr.show_text(user_text);
        
        // Instructions
        cr.set_source_rgb(0.3, 0.3, 0.4);
        let inst_text = "Press F11 for fullscreen • Move mouse to top to show controls";
        let inst_extents = cr.text_extents(inst_text).unwrap();
        let inst_x = (width as f64 - inst_extents.width()) / 2.0;
        cr.move_to(inst_x, y + 60.0);
        let _ = cr.show_text(inst_text);
    });

    // Handle window close
    let main_window_for_close = main_window.clone();
    rd_window.connect_close_request(move |_| {
        // Show the main window again when remote desktop closes
        main_window_for_close.set_visible(true);
        main_window_for_close.present();
        gtk::glib::Propagation::Proceed
    });

    // Show window and focus drawing area
    rd_window.present();
    drawing_area.grab_focus();
    
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
        let _password_text = password_input_clone.buffer().text();
        
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
                
                // Create the remote desktop window and pass the main window
                create_remote_desktop_window(&app_for_connection, &server_for_connection, &username_for_connection, &domain_for_connection, &main_window_for_connection);
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
}