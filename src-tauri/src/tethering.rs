//! Tethered shooting module for camera control
//! Provides libgphoto2 bindings for live capture and parameter monitoring

use gphoto2::{Context, Camera};
use gphoto2::camera::CameraEvent;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::Mutex;
use tauri::{AppHandle, Emitter};

use image as image_crate;
use rawler::{rawsource::RawSource, decoders::RawDecodeParams};
use chrono;

/// Current camera parameters with extended support
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraParams {
    pub iso: String,
    pub shutter_speed: String,
    pub aperture: String,
    pub exposure_compensation: Option<String>,
    pub shooting_mode: Option<String>,
    pub white_balance: Option<String>,
    pub focus_mode: Option<String>,
    pub drive_mode: Option<String>,
    pub metering_mode: Option<String>,
    pub battery_level: Option<f32>,
    pub images_remaining: Option<u32>,
    pub model: String,
    pub port: String,
}

/// Camera capture result - supports both single and dual capture (RAW+JPG)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureResult {
    pub file_path: String,
    pub raw_path: Option<String>,  // RAW file path (if captured separately)
    pub jpg_path: Option<String>,  // JPG file path (if captured separately)
    pub preview_path: Option<String>,
    pub width: u32,
    pub height: u32,
}

/// Global camera service state
pub struct CameraService {
    pub camera: Arc<Mutex<Option<Camera>>>,
    capture_dir: PathBuf,
    /// Current folder for downloading images from camera button presses
    current_download_folder: Arc<Mutex<Option<String>>>,
    /// Cached dimensions for faster capture (model -> (width, height))
    cached_dimensions: Arc<Mutex<std::collections::HashMap<String, (u32, u32)>>>,
}

impl CameraService {
    /// Create a new camera service
    pub fn new(capture_dir: PathBuf) -> Self {
        Self {
            camera: Arc::new(Mutex::new(None)),
            capture_dir,
            current_download_folder: Arc::new(Mutex::new(None)),
            cached_dimensions: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Extract real file extension from camera filename
    /// Handles formats like "capt0000.jpg", "IMG_1234.CR3", "CRW_0001.JPG", etc.
    fn extract_file_extension(original_name: &str) -> String {
        // Convert to lowercase for easier matching
        let name_lower = original_name.to_lowercase();

        // List of known RAW extensions
        let raw_extensions = ["cr3", "cr2", "nef", "arw", "dng", "raf", "orf", "pef", "rw2", "srw", "crw"];

        // Split by dots and process from right to left (last extension is the real one)
        let parts: Vec<&str> = name_lower.rsplit('.').collect();

        for (i, part) in parts.iter().enumerate() {
            if part.is_empty() {
                continue;
            }

            // Skip purely numeric parts or known camera internal prefixes
            // capt0000, 0000, etc. are camera's internal naming, not real extensions
            if part.chars().all(|c| c.is_numeric()) || part.starts_with("capt") {
                continue;
            }

            // Check if it's a known extension
            if *part == "jpg" || *part == "jpeg" || raw_extensions.contains(part) {
                return if *part == "jpeg" {
                    "jpg".to_string()
                } else {
                    part.to_string()
                };
            }

            // If we've gone past the first part (real extension) and hit something unknown,
            // and the previous parts were all camera-specific, return jpg as default
            if i > 0 {
                return "jpg".to_string();
            }
        }

        // Default to jpg if we can't determine
        "jpg".to_string()
    }

    /// Check if a file path is a RAW file
    fn is_raw_file(path: &str) -> bool {
        let path_lower = path.to_lowercase();
        path_lower.ends_with(".cr3")
            || path_lower.ends_with(".cr2")
            || path_lower.ends_with(".nef")
            || path_lower.ends_with(".arw")
            || path_lower.ends_with(".dng")
            || path_lower.ends_with(".raf")
            || path_lower.ends_with(".orf")
            || path_lower.ends_with(".pef")
            || path_lower.ends_with(".rw2")
            || path_lower.ends_with(".srw")
    }

    /// Get image dimensions, supporting both regular formats and RAW files
    fn get_image_dimensions(file_path: &PathBuf) -> Option<(u32, u32)> {
        // First try with image crate (for JPEG, PNG, etc.)
        if let Ok(dim) = image_crate::image_dimensions(file_path) {
            return Some(dim);
        }

        // If that fails and it's a RAW file, try with rawler
        if Self::is_raw_file(&file_path.to_string_lossy()) {
            if let Ok(data) = std::fs::read(file_path) {
                let source = RawSource::new_from_slice(&data);
                if let Ok(decoder) = rawler::get_decoder(&source) {
                    if let Ok(raw_image) = decoder.raw_image(&source, &RawDecodeParams::default(), false) {
                        let w = raw_image.width as u32;
                        let h = raw_image.height as u32;
                        return Some((w, h));
                    }
                }
            }
        }

        None
    }

    /// Helper to get a RadioWidget value with multiple key attempts
    fn get_radio_value(camera: &Camera, keys: &[&str]) -> Option<String> {
        for key in keys {
            if let Ok(widget) = camera.config_key::<gphoto2::widget::RadioWidget>(key).wait() {
                return Some(widget.choice().to_string());
            }
        }
        None
    }

    /// Connect to the first available camera
    pub async fn connect_camera(&self, app: AppHandle) -> std::result::Result<CameraParams, String> {
        let (camera, _model, _port) = tokio::task::spawn_blocking(|| {
            let context = Context::new().map_err(|e| format!("Failed to create context: {}", e))?;

            let camera = context.autodetect_camera()
                .wait()
                .map_err(|e| format!("Failed to autodetect: {}", e))?;

            // Get camera info
            let abilities = camera.abilities();
            let model = abilities.model().to_string();
            let port = "usb".to_string();

            Ok::<(Camera, String, String), String>((camera, model, port))
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))??;

        *self.camera.lock().await = Some(camera);

        // Get initial parameters
        let params = self.get_camera_params_internal().await?;

        // Emit connected event
        app.emit("camera:status", "Connected").ok();
        eprintln!("{} [Camera] Connected to {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), params.model);

        Ok(params)
    }

    /// Disconnect from current camera
    pub async fn disconnect_camera(&self, app: AppHandle) -> std::result::Result<(), String> {
        *self.camera.lock().await = None;
        app.emit("camera:status", "Disconnected").ok();
        eprintln!("{} [Camera] Disconnected by user", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
        Ok(())
    }

    /// Get current camera parameters (internal version with minimal logging)
    async fn get_camera_params_internal(&self) -> std::result::Result<CameraParams, String> {
        let camera = {
            let camera_guard = self.camera.lock().await;
            camera_guard
                .as_ref()
                .ok_or("No camera connected")?
                .clone()
        };

        let params = tokio::task::spawn_blocking(move || {
            let abilities = camera.abilities();
            let model = abilities.model().to_string();
            let port = "usb".to_string();

            // Get ISO - try multiple key names
            let iso = Self::get_radio_value(&camera, &["iso", "isospeed", "autoiso"])
                .ok_or_else(|| "Failed to get ISO - camera may be disconnected")?;

            // Get shutter speed
            let shutter_speed = Self::get_radio_value(&camera, &[
                "shutterspeed", "shutter", "shutterspeed2", "exptime", "exposuretime",
            ]).ok_or_else(|| "Failed to get shutter speed - camera may be disconnected")?;

            // Get aperture
            let aperture = Self::get_radio_value(&camera, &[
                "aperture", "f-number", "fnumber", "aperture2",
            ]).ok_or_else(|| "Failed to get aperture - camera may be disconnected")?;

            // Get other parameters (optional)
            let exposure_compensation = Self::get_radio_value(&camera, &[
                "exposurecompensation", "expcomp", "exposurecomp", "exposure",
            ]);

            let shooting_mode = Self::get_radio_value(&camera, &[
                "shootingmode", "capturemode", "capturemode2", "autoexposuremode", "exposuremode", "mode",
            ]);

            let white_balance = Self::get_radio_value(&camera, &[
                "whitebalance", "whitebalanceadjust", "whitebalance2", "wb",
            ]);

            let focus_mode = Self::get_radio_value(&camera, &[
                "focusmode", "autofocus", "afmode", "focusmode2",
            ]);

            let drive_mode = Self::get_radio_value(&camera, &[
                "drivemode", "capturemode", "continuous",
            ]);

            let metering_mode = Self::get_radio_value(&camera, &[
                "meteringmode", "meteringmodedial", "metering",
            ]);

            // Try to get battery level
            let battery_level = camera.config_key::<gphoto2::widget::RangeWidget>("batterylevel")
                .wait()
                .ok()
                .map(|w| w.value());

            // Try to get remaining images
            let images_remaining = camera.config_key::<gphoto2::widget::RangeWidget>("remainingimages")
                .wait()
                .ok()
                .map(|w| w.value() as u32);

            Ok::<CameraParams, String>(CameraParams {
                iso,
                shutter_speed,
                aperture,
                exposure_compensation,
                shooting_mode,
                white_balance,
                focus_mode,
                drive_mode,
                metering_mode,
                battery_level,
                images_remaining,
                model,
                port,
            })
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))??;

        Ok(params)
    }

    /// Get current camera parameters (public wrapper)
    pub async fn get_camera_params(&self) -> std::result::Result<CameraParams, String> {
        self.get_camera_params_internal().await
    }

    /// Get available choices for a configuration parameter
    pub async fn get_config_choices(&self, config_key: &str) -> std::result::Result<Vec<String>, String> {
        let camera = {
            let camera_guard = self.camera.lock().await;
            camera_guard
                .as_ref()
                .ok_or("No camera connected")?
                .clone()
        };

        let key = config_key.to_string();
        tokio::task::spawn_blocking(move || {
            let widget = camera.config_key::<gphoto2::widget::RadioWidget>(&key)
                .wait()
                .map_err(|e| format!("Failed to get config '{}': {}", key, e))?;

            let choices: Vec<String> = widget.choices_iter().map(|c| c.to_string()).collect();
            Ok(choices)
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }

    /// Set a configuration parameter value
    pub async fn set_config_value(&self, config_key: &str, value: &str) -> std::result::Result<(), String> {
        let camera = {
            let camera_guard = self.camera.lock().await;
            camera_guard
                .as_ref()
                .ok_or("No camera connected")?
                .clone()
        };

        let key = config_key.to_string();
        let value = value.to_string();
        tokio::task::spawn_blocking(move || {
            let widget = camera.config_key::<gphoto2::widget::RadioWidget>(&key)
                .wait()
                .map_err(|e| format!("Failed to get config '{}': {}", key, e))?;

            // Check if readonly
            if widget.readonly() {
                return Err(format!("Config '{}' is readonly", key));
            }

            widget.set_choice(&value)
                .map_err(|e| format!("Failed to set choice '{}' for '{}': {}", value, key, e))?;

            camera.set_config(&widget)
                .wait()
                .map_err(|e| format!("Failed to apply config '{}': {}", key, e))?;

            // Small delay to let camera process the change
            std::thread::sleep(std::time::Duration::from_millis(100));

            Ok(())
        })
        .await
        .map_err(|e| format!("Task join error: {}", e))?
    }

    /// Capture a photo and download it directly to target folder
    pub async fn capture_and_download(&self, app: AppHandle, target_folder: Option<String>) -> std::result::Result<CaptureResult, String> {
        let camera = {
            let camera_guard = self.camera.lock().await;
            camera_guard
                .as_ref()
                .ok_or("No camera connected")?
                .clone()
        };

        // Use target folder if provided, otherwise use default capture dir
        let capture_dir = if let Some(ref folder) = target_folder {
            // Store this as the current download folder for camera button captures
            *self.current_download_folder.lock().await = Some(folder.clone());
            std::path::PathBuf::from(folder)
        } else {
            self.capture_dir.clone()
        };

        // Add timeout to prevent blocking (60 seconds for camera to respond)
        let capture_result = tokio::time::timeout(
            tokio::time::Duration::from_secs(60),
            tokio::task::spawn_blocking(move || {
                eprintln!("{} [Camera] Capturing photo...", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
                // Capture with minimal retry logic
                let result = camera.capture_image().wait();
                let image_path = match result {
                    Ok(path) => path,
                    Err(e) => {
                        let error_msg = e.to_string().to_lowercase();
                        // Only retry on specific transient I/O errors
                        if error_msg.contains("i/o in progress") {
                            std::thread::sleep(std::time::Duration::from_secs(1));
                            let retry_result = camera.capture_image().wait();
                            match retry_result {
                                Ok(path) => path,
                                Err(retry_e) => {
                                    return Err(format!("Capture failed after retry: {}", retry_e));
                                }
                            }
                        } else {
                            return Err(format!("Capture failed: {}", e));
                        }
                    }
                };

                // Get file info
                let original_name = image_path.name();
                let ext = Self::extract_file_extension(&original_name);

                // Generate filename with timestamp
                let timestamp = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map_err(|e| format!("Time error: {}", e))?
                    .as_secs();

                let name = format!("capture_{:010}.{}", timestamp, ext);
                let file_path = capture_dir.join(&name);

                // Ensure capture directory exists
                std::fs::create_dir_all(&capture_dir)
                    .map_err(|e| format!("Failed to create capture directory: {}", e))?;

                // Download the file
                let fs = camera.fs();
                eprintln!("{} [Camera] Downloading file...", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
                fs.download_to(&image_path.folder(), &image_path.name(), &file_path)
                    .wait()
                    .map_err(|e| format!("Download failed: {}", e))?;
                eprintln!("{} [Camera] Downloaded to: {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), file_path.display());

                // Get dimensions - use cached value or quick check, fall back to default
                // For RAW files, use default dimensions immediately to avoid blocking
                let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
                let is_raw = matches!(ext.as_str(), "cr3" | "cr2" | "nef" | "arw" | "dng" | "raf" | "orf" | "pef" | "rw2" | "srw");

                // For RAW files, use default dimensions to avoid blocking
                // For JPEG, try to get actual dimensions quickly
                let dimensions = if is_raw {
                    // Use default dimensions for RAW - avoids slow rawler parsing
                    eprintln!("{} [Camera] Using default dimensions for RAW file", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
                    (1920, 1080)
                } else {
                    // For JPEG, quick image crate check
                    Self::get_image_dimensions(&file_path).unwrap_or((1920, 1080))
                };

                Ok::<(PathBuf, u32, u32), String>((file_path, dimensions.0, dimensions.1))
            })
        ).await
        .map_err(|e| format!("Task join error: {}", e))?;  // Handle JoinError

        // Handle both timeout and capture errors
        let (file_path, width, height) = match capture_result {
            Ok(inner_result) => inner_result.map_err(|e| format!("Capture error: {}", e))?,
            Err(_) => return Err("Capture timeout after 60 seconds. Camera may be disconnected or busy.".to_string()),
        };

        // Emit capture complete event
        app.emit("camera:captured", serde_json::json!({
            "filePath": file_path.to_string_lossy().to_string(),
            "width": width,
            "height": height,
        })).ok();

        Ok(CaptureResult {
            file_path: file_path.to_string_lossy().to_string(),
            raw_path: None,
            jpg_path: None,
            preview_path: None,
            width,
            height,
        })
    }

    /// Auto-detect and connect to camera (hot-plug support)
    pub async fn auto_connect(&self, app: AppHandle) -> std::result::Result<CameraParams, String> {
        // Try to detect camera with multiple attempts
        for attempt in 1..=5 {
            let result: std::result::Result<Option<(Camera, String)>, String> = tokio::task::spawn_blocking(move || {
                let context = Context::new().map_err(|e| format!("Failed to create context: {}", e))?;

                // Try to autodetect
                match context.autodetect_camera().wait() {
                    Ok(camera) => {
                        let abilities = camera.abilities();
                        let model = abilities.model().to_string();
                        Ok::<Option<(Camera, String)>, String>(Some((camera, model)))
                    }
                    Err(e) => {
                        let error_msg = e.to_string().to_lowercase();
                        if error_msg.contains("could not claim") || error_msg.contains("usb") {
                            Err(format!("USB occupied - close other camera apps"))
                        } else {
                            Ok(None)
                        }
                    }
                }
            })
            .await
            .map_err(|e| format!("Task join error: {}", e))?;

            if let Ok(Some((camera, _model))) = result {
                // Store camera
                *self.camera.lock().await = Some(camera);

                // Verify connection by actually getting params
                match self.get_camera_params_internal().await {
                    Ok(params) => {
                        app.emit("camera:status", "Connected").ok();
                        return Ok(params);
                    }
                    Err(_e) => {
                        *self.camera.lock().await = None;
                        // Continue to next attempt
                    }
                }
            }

            if attempt < 5 {
                tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
            }
        }

        Err("No camera detected".to_string())
    }

    /// Start background monitoring for camera connection
    pub async fn start_monitoring(self: Arc<Self>, app: AppHandle) -> std::result::Result<(), String> {
        // Track if event monitoring is running to avoid duplicate spawns
        use std::sync::atomic::{AtomicBool, Ordering};
        let event_monitoring_active = Arc::new(AtomicBool::new(false));
        let event_monitoring_active_clone = event_monitoring_active.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_millis(500));
            let mut was_connected = false;
            loop {
                interval.tick().await;

                // Check if camera is connected
                let is_connected = self.camera.lock().await.is_some();

                if !is_connected {
                    was_connected = false;
                    // Camera not connected - try to auto-connect
                    let _ = self.auto_connect(app.clone()).await;
                } else {
                    // Camera is connected
                    // Start event monitoring if it wasn't running before (reconnect scenario)
                    if !was_connected && !event_monitoring_active_clone.load(Ordering::Relaxed) {
                        event_monitoring_active_clone.store(true, Ordering::Relaxed);
                        let self_clone = self.clone();
                        let app_clone = app.clone();
                        let active_flag = event_monitoring_active_clone.clone();
                        tokio::spawn(async move {
                            self_clone.start_event_monitoring_with_flag(app_clone, active_flag).await;
                        });
                    }
                    was_connected = true;

                    // Camera is connected, verify it's still responsive
                    match self.get_camera_params().await {
                        Ok(_) => {}
                        Err(e) => {
                            // Check if this is a disconnection error (PTP/IO errors)
                            let error_msg = e.to_string().to_lowercase();
                            let is_disconnect_error = error_msg.contains("ptp")
                                || error_msg.contains("i/o")
                                || error_msg.contains("could not")
                                || error_msg.contains("not found")
                                || error_msg.contains("timeout")
                                || error_msg.contains("unspecified");

                            // Immediate disconnect on first critical error
                            if is_disconnect_error {
                                eprintln!("{} [Camera] Disconnected: {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), e);
                                *self.camera.lock().await = None;
                                let _ = app.emit("camera:status", "Disconnected");
                                was_connected = false;
                            }
                        }
                    }
                }
            }
        });

        Ok(())
    }

    /// Download a file from the camera and return the result
    async fn download_camera_file(
        &self,
        camera: Camera,
        folder: String,
        name: String,
        capture_dir: PathBuf,
    ) -> std::result::Result<(String, u32, u32), String> {
        let ext = Self::extract_file_extension(&name);

        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|e| format!("Time error: {}", e))?
            .as_secs();

        let new_name = format!("capture_{:010}.{}", timestamp, ext);
        let file_path = capture_dir.join(&new_name);

        // Ensure capture directory exists
        std::fs::create_dir_all(&capture_dir)
            .map_err(|e| format!("Failed to create capture directory: {}", e))?;

        // Get camera model for cache lookup
        let camera_model = camera.abilities().model().to_string();

        // Check cache first for faster response
        let dimensions = {
            let cache = self.cached_dimensions.lock().await;
            cache.get(&camera_model).copied()
        };

        // Use camera filesystem to download the file
        let fs = camera.fs();
        eprintln!("{} [Camera] Downloading from camera button...", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
        fs.download_to(&folder, &name, &file_path)
            .wait()
            .map_err(|e| format!("Download failed: {}", e))?;
        eprintln!("{} [Camera] Downloaded to: {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), file_path.display());

        // Get dimensions - use cached value if available, otherwise parse and cache
        let dimensions = if let Some(dim) = dimensions {
            dim
        } else {
            // Parse and cache for next time
            let dim = Self::get_image_dimensions(&file_path)
                .unwrap_or((1920, 1080));
            // Cache for next time
            {
                let mut cache = self.cached_dimensions.lock().await;
                cache.insert(camera_model.clone(), dim);
            }
            dim
        };

        Ok((file_path.to_string_lossy().to_string(), dimensions.0, dimensions.1))
    }

    /// Start monitoring camera events (for camera button captures)
    pub fn start_event_monitoring(self: Arc<Self>, app: AppHandle) {
        tokio::spawn(async move {
            self.start_event_monitoring_inner(app.clone(), None).await;
        });
    }

    /// Start monitoring camera events with a flag that can be used for reconnection tracking
    async fn start_event_monitoring_with_flag(self: Arc<Self>, app: AppHandle, active_flag: Arc<std::sync::atomic::AtomicBool>) {
        self.start_event_monitoring_inner(app.clone(), Some(active_flag)).await;
    }

    /// Inner event monitoring implementation
    async fn start_event_monitoring_inner(self: Arc<Self>, app: AppHandle, active_flag: Option<Arc<std::sync::atomic::AtomicBool>>) {
        let mut event_interval = tokio::time::interval(Duration::from_millis(100));
        loop {
            event_interval.tick().await;

            // Check if camera is connected
            let camera_opt = {
                let guard = self.camera.lock().await;
                guard.clone()
            };

            if let Some(camera) = camera_opt {
                // Clone camera for use in event monitoring
                let camera_clone = camera.clone();

                // Check for events - wrapped in catch_unwind to handle gphoto2 crashes
                let event_result = tokio::task::spawn_blocking(move || {
                    // Wrap in catch_unwind to recover from gphoto2 library crashes
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        camera_clone.wait_event(Duration::from_millis(300)).wait()
                    }))
                })
                .await;

                // Handle the result, including potential panics
                let event = match event_result {
                    Ok(Ok(Ok(event))) => Some(event),
                    Ok(Ok(Err(e))) => {
                        // gphoto2 returned an error
                        let error_msg = e.to_string().to_lowercase();

                        // Check if camera is disconnected
                        // "Unspecified error" (0x2002) often happens when camera is disconnected
                        // "Could not find the requested device on the USB port" indicates USB disconnect
                        if error_msg.contains("no device")
                            || error_msg.contains("not found")
                            || error_msg.contains("disconnected")
                            || error_msg.contains("i/o error")
                            || error_msg.contains("unspecified")
                            || error_msg.contains("general error")
                            || error_msg.contains("usb port") {
                            eprintln!("{} [Camera] Disconnected", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
                            // Clear camera and emit disconnect event
                            {
                                let mut camera_guard = self.camera.lock().await;
                                *camera_guard = None;
                            }
                            let _ = app.emit("camera:status", "Disconnected");
                            // Clear the active flag so monitoring can be restarted
                            if let Some(flag) = active_flag {
                                flag.store(false, std::sync::atomic::Ordering::Relaxed);
                            }
                            // Break the loop to stop monitoring
                            break;
                        }

                        None
                    }
                    Ok(Err(_panic_info)) => {
                        // A panic occurred in the wait_event call (likely gphoto2 segfault)
                        eprintln!("{} [Camera] Thread panic - disconnected", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
                        // Clear camera and emit disconnect event
                        {
                            let mut camera_guard = self.camera.lock().await;
                            *camera_guard = None;
                        }
                        let _ = app.emit("camera:status", "Disconnected");
                        // Clear the active flag so monitoring can be restarted
                        if let Some(flag) = active_flag {
                            flag.store(false, std::sync::atomic::Ordering::Relaxed);
                        }
                        // Break the loop to stop monitoring
                        break;
                    }
                    Err(join_error) => {
                        // Task failed to join
                        eprintln!("{} [Camera] Event monitoring task failed: {:?}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), join_error);
                        // Clear the active flag so monitoring can be restarted
                        if let Some(flag) = active_flag {
                            flag.store(false, std::sync::atomic::Ordering::Relaxed);
                        }
                        // Break the loop on task failure
                        break;
                    }
                };

                if let Some(event) = event {
                    match event {
                        CameraEvent::NewFile(file_path) => {
                            // Get current download folder
                            let download_folder = self.current_download_folder.lock().await.clone();
                            let capture_dir = if let Some(folder) = download_folder {
                                std::path::PathBuf::from(folder)
                            } else {
                                self.capture_dir.clone()
                            };

                            let folder_str = file_path.folder().to_string();
                            let name_str = file_path.name().to_string();

                            // Spawn background download task
                            let self_clone = self.clone();
                            let app_clone = app.clone();
                            tokio::spawn(async move {
                                if let Ok((file_path, width, height)) = self_clone.download_camera_file(
                                    camera,
                                    folder_str,
                                    name_str,
                                    capture_dir,
                                ).await {
                                    app_clone.emit("camera:captured", serde_json::json!({
                                        "filePath": file_path,
                                        "width": width,
                                        "height": height,
                                    })).ok();
                                }
                            });
                        }
                        CameraEvent::CaptureComplete => {}
                        CameraEvent::Timeout => {}
                        CameraEvent::Unknown(_) => {}
                        CameraEvent::FileChanged(_) => {}
                        CameraEvent::NewFolder(_) => {}
                    }
                }
            } else {
                // Camera disconnected, clear flag and exit
                if let Some(flag) = active_flag {
                    flag.store(false, std::sync::atomic::Ordering::Relaxed);
                }
                break;
            }
        }
    }
}

// ============================================================================
// Tauri Commands
// ============================================================================

/// Connect to a camera
#[tauri::command]
pub async fn tether_connect(
    service: tauri::State<'_, CameraService>,
    app: AppHandle,
) -> std::result::Result<CameraParams, String> {
    service.connect_camera(app).await
}

/// Disconnect from camera
#[tauri::command]
pub async fn tether_disconnect(
    service: tauri::State<'_, CameraService>,
    app: AppHandle,
) -> std::result::Result<(), String> {
    service.disconnect_camera(app).await
}

/// Get current camera parameters
#[tauri::command]
pub async fn tether_get_params(
    service: tauri::State<'_, CameraService>,
) -> std::result::Result<CameraParams, String> {
    service.get_camera_params().await
}

/// Capture a photo
#[tauri::command]
pub async fn tether_capture(
    service: tauri::State<'_, CameraService>,
    app: AppHandle,
    target_folder: Option<String>,
) -> std::result::Result<CaptureResult, String> {
    service.capture_and_download(app, target_folder).await
}

/// Start background monitoring
#[tauri::command]
pub async fn tether_start_monitoring(
    service: tauri::State<'_, CameraService>,
    app: AppHandle,
) -> std::result::Result<(), String> {
    // Create a new Arc wrapper that shares the same inner state
    let service_arc = Arc::new(CameraService {
        camera: service.camera.clone(),
        capture_dir: service.capture_dir.clone(),
        current_download_folder: service.current_download_folder.clone(),
        cached_dimensions: service.cached_dimensions.clone(),
    });

    // Start both connection monitoring and event monitoring
    service_arc.clone().start_monitoring(app.clone()).await?;
    service_arc.start_event_monitoring(app);

    Ok(())
}

/// Set current download folder for camera button captures
#[tauri::command]
pub async fn tether_set_download_folder(
    service: tauri::State<'_, CameraService>,
    folder: String,
) -> std::result::Result<(), String> {
    *service.current_download_folder.lock().await = Some(folder);
    Ok(())
}

/// Get available choices for a camera configuration parameter
#[tauri::command]
pub async fn tether_get_config_choices(
    service: tauri::State<'_, CameraService>,
    config_key: String,
) -> std::result::Result<Vec<String>, String> {
    service.get_config_choices(&config_key).await
}

/// Set a camera configuration parameter value
#[tauri::command]
pub async fn tether_set_config_value(
    service: tauri::State<'_, CameraService>,
    config_key: String,
    value: String,
) -> std::result::Result<(), String> {
    service.set_config_value(&config_key, &value).await
}
