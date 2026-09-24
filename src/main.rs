use openaction::*;

use base64::Engine;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

const CACHE_FILE_NAME: &str = "rate_limits.json";
const CACHE_DIR_NAME: &str = "oaclaudecode-usage";

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
struct UsageSettings {
	mode: String,
	show_percent: bool,
}

impl Default for UsageSettings {
	fn default() -> Self {
		Self {
			mode: "session".to_string(),
			show_percent: true,
		}
	}
}

/// Per-instance state tracked by the plugin so the background ticker can
/// refresh every visible key without waiting on the next inbound event.
#[derive(Clone, Default)]
struct InstanceState {
	settings: UsageSettings,
}

static INSTANCES: LazyLock<Mutex<HashMap<String, InstanceState>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// A single rate-limit window as reported by Claude Code's statusLine hook
/// (`rate_limits.five_hour` / `rate_limits.seven_day`).
#[derive(Deserialize, Serialize, Clone, Copy)]
struct RateWindow {
	used_percentage: f64,
	resets_at: i64,
}

#[derive(Deserialize, Default)]
struct RateLimits {
	five_hour: Option<RateWindow>,
	seven_day: Option<RateWindow>,
}

/// The subset of Claude Code's statusLine JSON payload we care about.
#[derive(Deserialize)]
struct StatusLinePayload {
	rate_limits: Option<RateLimits>,
}

/// What we persist to disk, merged across statusLine invocations so a single
/// invocation reporting only one window doesn't erase the other.
#[derive(Serialize, Deserialize, Default)]
struct CacheFile {
	five_hour: Option<RateWindow>,
	seven_day: Option<RateWindow>,
	updated_at: i64,
}

impl CacheFile {
	fn get(&self, mode: &str) -> Option<RateWindow> {
		match mode {
			"weekly" => self.seven_day,
			_ => self.five_hour,
		}
	}
}

fn cache_file_path() -> PathBuf {
	let base = dirs::cache_dir().unwrap_or_else(std::env::temp_dir);
	base.join(CACHE_DIR_NAME).join(CACHE_FILE_NAME)
}

fn format_remaining(secs: i64) -> String {
	if secs < 60 {
		return "now".to_string();
	}
	let mins = secs / 60;
	if mins < 60 {
		return format!("{} min.", mins);
	}
	let hours = mins / 60;
	if hours < 24 {
		return format!("{} hr", hours);
	}
	let days = hours / 24;
	format!("{} day{}", days, if days == 1 { "" } else { "s" })
}

fn color_for(percent: f64) -> &'static str {
	if percent < 50.0 {
		"#2ecc71"
	} else if percent < 85.0 {
		"#f1c40f"
	} else {
		"#e74c3c"
	}
}

fn render_svg(mode: &str, percent: f64, resets_at: i64, now: i64, show_percent: bool) -> String {
	let color = color_for(percent);
	let countdown = format_remaining((resets_at - now).max(0));

	let radius = 56.0;
	let circumference = 2.0 * std::f64::consts::PI * radius;
	let offset = circumference * (1.0 - (percent / 100.0).clamp(0.0, 1.0));

	let percent_text = format!("{:.0}%", percent);
	let mode_label = match mode {
		"weekly" => "WEEKLY",
		_ => "SESSION",
	};
	let percent_line = if show_percent {
		format!(r#"  <text x="72" y="102" text-anchor="middle" fill="{color}" font-family="sans-serif" font-size="14" font-weight="600">{percent_text}</text>"#)
	} else {
		String::new()
	};

	format!(
		r##"<svg xmlns="http://www.w3.org/2000/svg" width="144" height="144" viewBox="0 0 144 144">
  <circle cx="72" cy="72" r="{r}" fill="none" stroke="#3a3a3a" stroke-width="12"/>
  <circle cx="72" cy="72" r="{r}" fill="none" stroke="{color}" stroke-width="12" stroke-linecap="round"
     stroke-dasharray="{circ}" stroke-dashoffset="{offset}"
     transform="rotate(-90 72 72)"/>
  <text x="72" y="56" text-anchor="middle" fill="#9a9a9a" font-family="sans-serif" font-size="11" font-weight="600">{mode_label}</text>
  <text x="72" y="80" text-anchor="middle" fill="#ffffff" font-family="sans-serif" font-size="24" font-weight="bold">{countdown}</text>
{percent_line}
</svg>"##,
		r = radius,
		circ = circumference,
		offset = offset,
		color = color,
		mode_label = mode_label,
		countdown = countdown,
		percent_line = percent_line,
	)
}

fn render_svg_error(message: &str) -> String {
	let short: String = message.chars().take(16).collect();
	format!(
		r##"<svg xmlns="http://www.w3.org/2000/svg" width="144" height="144" viewBox="0 0 144 144">
  <text x="72" y="76" text-anchor="middle" fill="#e74c3c" font-family="sans-serif" font-size="18" font-weight="bold">err</text>
  <text x="72" y="98" text-anchor="middle" fill="#9a9a9a" font-family="sans-serif" font-size="12">{short}</text>
</svg>"##,
		short = short,
	)
}

fn render_svg_waiting() -> String {
	r##"<svg xmlns="http://www.w3.org/2000/svg" width="144" height="144" viewBox="0 0 144 144">
  <text x="72" y="68" text-anchor="middle" fill="#f1c40f" font-family="sans-serif" font-size="16" font-weight="bold">NO DATA</text>
  <text x="72" y="90" text-anchor="middle" fill="#9a9a9a" font-family="sans-serif" font-size="12">open Claude Code</text>
</svg>"##
		.to_string()
}

fn to_data_uri(svg: &str) -> String {
	let b64 = base64::engine::general_purpose::STANDARD.encode(svg.as_bytes());
	format!("data:image/svg+xml;base64,{}", b64)
}

async fn send_image(instance: &Instance, svg: &str, label: &str) {
	let uri = to_data_uri(svg);
	log::debug!(
		"[{}] sending {label} image (uri len {})\nsvg: {}\nuri prefix: {}",
		instance.instance_id,
		uri.len(),
		svg,
		&uri[..uri.len().min(40)],
	);
	let result = instance.set_image(Some(uri), None).await;
	if let Err(e) = result {
		log::error!("[{}] set_image failed: {}", instance.instance_id, e);
	}
}

/// Outcome of trying to read a rate-limit window out of the cache file
/// written by the statusLine bridge.
enum CacheLookup {
	Found { percent: f64, resets_at: i64 },
	Waiting,
	Error(String),
}

fn lookup_window(cache: &CacheFile, mode: &str, now: i64) -> CacheLookup {
	match cache.get(mode) {
		Some(window) if window.resets_at > now => CacheLookup::Found {
			percent: window.used_percentage,
			resets_at: window.resets_at,
		},
		Some(_) => {
			log::debug!("lookup_window: mode={} window expired without refresh", mode);
			CacheLookup::Waiting
		}
		None => {
			log::debug!("lookup_window: mode={} not present in cache", mode);
			CacheLookup::Waiting
		}
	}
}

fn read_cache(mode: &str) -> CacheLookup {
	let path = cache_file_path();
	let bytes = match std::fs::read(&path) {
		Ok(b) => b,
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
			log::debug!("read_cache: no cache file yet at {}", path.display());
			return CacheLookup::Waiting;
		}
		Err(e) => {
			log::warn!("read_cache: could not read {}: {}", path.display(), e);
			return CacheLookup::Error(e.to_string());
		}
	};

	let cache: CacheFile = match serde_json::from_slice(&bytes) {
		Ok(c) => c,
		Err(e) => {
			log::warn!("read_cache: json parse failed: {}", e);
			return CacheLookup::Error(e.to_string());
		}
	};

	lookup_window(&cache, mode, chrono::Utc::now().timestamp())
}

async fn set_usage_image(instance: &Instance, state: &InstanceState) {
	let now = chrono::Utc::now().timestamp();
	log::debug!("[{}] set_usage_image: mode={}", instance.instance_id, state.settings.mode);

	match read_cache(&state.settings.mode) {
		CacheLookup::Found { percent, resets_at } => {
			let svg = render_svg(&state.settings.mode, percent, resets_at, now, state.settings.show_percent);
			send_image(instance, &svg, "usage").await;
		}
		CacheLookup::Waiting => {
			send_image(instance, &render_svg_waiting(), "waiting").await;
		}
		CacheLookup::Error(e) => {
			send_image(instance, &render_svg_error(&e), "error").await;
		}
	}
}

struct UsageAction;

#[async_trait]
impl Action for UsageAction {
	const UUID: ActionUuid = "com.esteban-dcp.claudecodeusage.usage";
	type Settings = UsageSettings;

	async fn will_appear(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
		log::debug!("[{}] will_appear: mode={}", instance.instance_id, settings.mode);
		let id = instance.instance_id.clone();
		let state = {
			let mut map = INSTANCES.lock().unwrap();
			let entry = map.entry(id.clone()).or_default();
			entry.settings = settings.clone();
			entry.clone()
		};

		set_usage_image(instance, &state).await;
		Ok(())
	}

	async fn will_disappear(&self, instance: &Instance, _settings: &Self::Settings) -> OpenActionResult<()> {
		log::debug!("[{}] will_disappear", instance.instance_id);
		INSTANCES.lock().unwrap().remove(&instance.instance_id);
		Ok(())
	}

	async fn did_receive_settings(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
		log::debug!("[{}] did_receive_settings: mode={}", instance.instance_id, settings.mode);
		let id = instance.instance_id.clone();
		let state = {
			let mut map = INSTANCES.lock().unwrap();
			let entry = map.entry(id.clone()).or_default();
			entry.settings = settings.clone();
			entry.clone()
		};

		set_usage_image(instance, &state).await;
		Ok(())
	}

	async fn key_up(&self, instance: &Instance, settings: &Self::Settings) -> OpenActionResult<()> {
		// Manual refresh on press; the cache file is the source of truth so
		// this just re-reads and re-renders.
		log::debug!("[{}] key_up: manual refresh", instance.instance_id);
		let state = InstanceState { settings: settings.clone() };
		set_usage_image(instance, &state).await;
		Ok(())
	}
}

async fn tick_instances() {
	let visible = visible_instances(UsageAction::UUID).await;

	let snapshot: Vec<(std::sync::Arc<Instance>, InstanceState)> = {
		let map = INSTANCES.lock().unwrap();
		visible
			.into_iter()
			.filter_map(|instance| {
				let id = instance.instance_id.clone();
				map.get(&id).map(|state| (instance, state.clone()))
			})
			.collect()
	};
	log::debug!("tick_instances: {} visible instances", snapshot.len());

	for (instance, state) in snapshot {
		set_usage_image(&instance, &state).await;
	}
}

/// Merges freshly reported rate-limit windows into the existing cache,
/// keeping a previously cached window when this invocation didn't report it
/// (each statusLine event may only carry one window).
fn merge_cache(mut existing: CacheFile, incoming: RateLimits) -> CacheFile {
	if incoming.five_hour.is_some() {
		existing.five_hour = incoming.five_hour;
	}
	if incoming.seven_day.is_some() {
		existing.seven_day = incoming.seven_day;
	}
	existing.updated_at = chrono::Utc::now().timestamp();
	existing
}

/// Reads Claude Code's statusLine JSON from stdin, merges any reported
/// rate-limit windows into the cache file, and prints a short fallback
/// status line so the hook stays useful even without OpenDeck running.
fn run_statusline_bridge() -> i32 {
	let mut input = String::new();
	if let Err(e) = std::io::stdin().read_to_string(&mut input) {
		eprintln!("oaclaudecode-usage: failed to read stdin: {}", e);
		return 1;
	}

	let payload: StatusLinePayload = match serde_json::from_str(&input) {
		Ok(p) => p,
		Err(e) => {
			eprintln!("oaclaudecode-usage: failed to parse statusline payload: {}", e);
			return 1;
		}
	};

	let path = cache_file_path();
	if let Some(parent) = path.parent() {
		if let Err(e) = std::fs::create_dir_all(parent) {
			eprintln!("oaclaudecode-usage: failed to create cache dir: {}", e);
			return 1;
		}
	}

	let existing: CacheFile = std::fs::read(&path)
		.ok()
		.and_then(|b| serde_json::from_slice(&b).ok())
		.unwrap_or_default();

	let cache = merge_cache(existing, payload.rate_limits.unwrap_or_default());

	match serde_json::to_vec(&cache) {
		Ok(bytes) => {
			if let Err(e) = std::fs::write(&path, bytes) {
				eprintln!("oaclaudecode-usage: failed to write cache file: {}", e);
				return 1;
			}
		}
		Err(e) => {
			eprintln!("oaclaudecode-usage: failed to serialize cache: {}", e);
			return 1;
		}
	}

	let session = cache.five_hour.map(|w| format!("{:.0}%", w.used_percentage)).unwrap_or_else(|| "--".to_string());
	let weekly = cache.seven_day.map(|w| format!("{:.0}%", w.used_percentage)).unwrap_or_else(|| "--".to_string());
	println!("Session {} · Week {}", session, weekly);
	0
}

#[tokio::main]
async fn main() -> OpenActionResult<()> {
	let args: Vec<String> = std::env::args().collect();
	if args.get(1).map(String::as_str) == Some("--statusline-bridge") {
		std::process::exit(run_statusline_bridge());
	}

	{
		use simplelog::*;
		if let Err(error) = TermLogger::init(
			LevelFilter::Debug,
			Config::default(),
			TerminalMode::Stdout,
			ColorChoice::Never,
		) {
			eprintln!("Logger initialization failed: {}", error);
		}
	}

	register_action(UsageAction).await;

	// Background refresher: update countdown text every 30s and re-read the
	// cache file written by the statusLine bridge.
	let ticker = tokio::spawn(async {
		log::debug!("background ticker started (30s interval)");
		let mut interval = tokio::time::interval(Duration::from_secs(30));
		loop {
			interval.tick().await;
			tick_instances().await;
		}
	});

	let result = run(args).await;
	ticker.abort();
	result
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn countdown_minutes() {
		assert_eq!(format_remaining(45), "now");
		assert_eq!(format_remaining(6 * 60), "6 min.");
	}

	#[test]
	fn countdown_hours() {
		assert_eq!(format_remaining(2 * 3600), "2 hr");
		assert_eq!(format_remaining(3600), "1 hr");
	}

	#[test]
	fn countdown_days() {
		assert_eq!(format_remaining(24 * 3600), "1 day");
		assert_eq!(format_remaining(5 * 24 * 3600), "5 days");
	}

	#[test]
	fn parses_statusline_payload() {
		// Mirrors the real statusLine JSON's rate_limits object.
		let body = r#"{
			"rate_limits": {
				"five_hour": { "used_percentage": 23.5, "resets_at": 1738425600 },
				"seven_day": { "used_percentage": 41.2, "resets_at": 1738857600 }
			}
		}"#;
		let parsed: StatusLinePayload = serde_json::from_str(body).expect("sample payload must deserialize");
		let limits = parsed.rate_limits.expect("rate_limits present");
		assert_eq!(limits.five_hour.unwrap().used_percentage, 23.5);
		assert_eq!(limits.seven_day.unwrap().resets_at, 1738857600);
	}

	#[test]
	fn parses_statusline_payload_missing_rate_limits() {
		// rate_limits is absent until the first API response of the session.
		let body = r#"{"session_id": "abc123"}"#;
		let parsed: StatusLinePayload = serde_json::from_str(body).expect("payload without rate_limits must deserialize");
		assert!(parsed.rate_limits.is_none());
	}

	#[test]
	fn lookup_window_waiting_when_absent() {
		let cache = CacheFile { five_hour: None, seven_day: None, updated_at: 0 };
		assert!(matches!(lookup_window(&cache, "session", 1000), CacheLookup::Waiting));
	}

	#[test]
	fn lookup_window_waiting_when_expired() {
		let cache = CacheFile {
			five_hour: Some(RateWindow { used_percentage: 90.0, resets_at: 500 }),
			seven_day: None,
			updated_at: 0,
		};
		// resets_at (500) is in the past relative to now (1000): expired.
		assert!(matches!(lookup_window(&cache, "session", 1000), CacheLookup::Waiting));
	}

	#[test]
	fn lookup_window_found_when_fresh() {
		let cache = CacheFile {
			five_hour: Some(RateWindow { used_percentage: 30.0, resets_at: 2000 }),
			seven_day: None,
			updated_at: 0,
		};
		match lookup_window(&cache, "session", 1000) {
			CacheLookup::Found { percent, resets_at } => {
				assert_eq!(percent, 30.0);
				assert_eq!(resets_at, 2000);
			}
			_ => panic!("expected Found"),
		}
	}

	#[test]
	fn cache_get_selects_window_by_mode() {
		let cache = CacheFile {
			five_hour: Some(RateWindow { used_percentage: 10.0, resets_at: 100 }),
			seven_day: Some(RateWindow { used_percentage: 20.0, resets_at: 200 }),
			updated_at: 0,
		};
		assert_eq!(cache.get("session").unwrap().used_percentage, 10.0);
		assert_eq!(cache.get("weekly").unwrap().used_percentage, 20.0);
	}

	#[test]
	fn merge_preserves_window_not_reported_this_invocation() {
		let existing = CacheFile {
			five_hour: Some(RateWindow { used_percentage: 10.0, resets_at: 100 }),
			seven_day: Some(RateWindow { used_percentage: 20.0, resets_at: 200 }),
			updated_at: 0,
		};
		// This event only reports five_hour; seven_day must survive untouched.
		let incoming = RateLimits {
			five_hour: Some(RateWindow { used_percentage: 15.0, resets_at: 150 }),
			seven_day: None,
		};
		let merged = merge_cache(existing, incoming);
		assert_eq!(merged.five_hour.unwrap().used_percentage, 15.0);
		assert_eq!(merged.seven_day.unwrap().used_percentage, 20.0);
	}

	#[test]
	fn colors() {
		assert_eq!(color_for(49.9), "#2ecc71");
		assert_eq!(color_for(50.0), "#f1c40f");
		assert_eq!(color_for(84.9), "#f1c40f");
		assert_eq!(color_for(85.0), "#e74c3c");
		assert_eq!(color_for(100.0), "#e74c3c");
	}

	#[test]
	fn ring_geometry() {
		let svg = render_svg("weekly", 25.0, 1_000_000, 0, true);
		assert!(svg.contains("stroke-dasharray=\"351.858"));
		assert!(svg.contains("#2ecc71"));
		assert!(svg.contains("WEEKLY"));
		assert!(svg.contains("11 day"));
	}

	#[test]
	fn session_mode_label() {
		let svg = render_svg("session", 10.0, 1_000_000, 0, true);
		assert!(svg.contains("SESSION"));
	}

	#[test]
	fn full_ring_has_zero_offset_arc() {
		// At 100%, the arc covers the whole ring (offset 0).
		let svg = render_svg("session", 100.0, 1_000_000, 0, true);
		assert!(svg.contains("stroke-dashoffset=\"0\""));
	}

	#[test]
	fn percent_toggle_shows_and_hides_percent_text() {
		let with_percent = render_svg("session", 42.0, 1_000_000, 0, true);
		assert!(with_percent.contains(">42%</text>"));
		assert!(with_percent.contains("font-size=\"24\""));

		let without_percent = render_svg("session", 42.0, 1_000_000, 0, false);
		assert!(!without_percent.contains("42%"));
		// Layout and countdown size are unchanged when the toggle is off.
		assert!(without_percent.contains("font-size=\"24\""));
	}

	#[test]
	fn data_uri_is_svg() {
		let uri = to_data_uri(&render_svg_waiting());
		assert!(uri.starts_with("data:image/svg+xml;base64,"));
	}

	#[test]
	fn error_truncation_is_char_safe() {
		let svg = render_svg_error("this is a very long message that should be truncated!");
		assert!(svg.contains("this is a very l"));
	}
}
