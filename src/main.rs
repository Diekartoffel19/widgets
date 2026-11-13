#![windows_subsystem = "windows"] // keine Konsole unter Windows

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::{Local, Locale, NaiveDate};
use eframe::egui::{self, Color32, FontFamily, FontId, Pos2, Sense, Vec2, ViewportCommand};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Datenmodell
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize, Deserialize, PartialEq)]
enum Kind {
    Clock { format: String },
    Date { format: String },
    Text { text: String },
    System,
    Countdown { label: String, target: String },
    /// Freihand-Zeichnung; Stiftfarbe und -breite gelten für neue Linien.
    Draw { lines: Vec<Line>, pen_color: [u8; 4], pen_width: f32 },
}

/// Eine Freihand-Linie. Punkte sind relativ zur linken oberen Ecke des Widgets.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
struct Line {
    color: [u8; 4],
    width: f32,
    points: Vec<[f32; 2]>,
}

impl Kind {
    fn label(&self) -> &'static str {
        match self {
            Kind::Clock { .. } => "Uhr",
            Kind::Date { .. } => "Datum",
            Kind::Text { .. } => "Text",
            Kind::System => "System",
            Kind::Countdown { .. } => "Countdown",
            Kind::Draw { .. } => "Zeichnung",
        }
    }
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq)]
enum Align {
    Left,
    Center,
    Right,
}

// Fehlende Felder (z. B. aus einer älteren Version) bekommen Standardwerte,
// statt dass die ganze Datei nicht mehr lesbar ist.
#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
struct WidgetCfg {
    id: u64,
    name: String,
    kind: Kind,
    visible: bool,
    pos: [f32; 2],
    size: [f32; 2],
    auto_size: bool,
    font_size: f32,
    monospace: bool,
    text_color: [u8; 4],
    bg_color: [u8; 4],
    border_color: [u8; 4],
    border_width: f32,
    rounding: f32,
    padding: f32,
    align: Align,
    /// Deckkraft des ganzen Widgets: Text, Hintergrund, Rahmen und Linien.
    opacity: f32,
    /// Schatten hinter dem Text, damit er auch ohne Hintergrund lesbar bleibt.
    text_shadow: bool,
    /// Vom Editor gesetzt, wenn das Fenster verschoben werden soll.
    #[serde(skip)]
    move_requested: bool,
}

impl WidgetCfg {
    fn new(id: u64, kind: Kind) -> Self {
        let name = format!("{} {}", kind.label(), id);
        let font_size = match kind {
            Kind::Clock { .. } => 48.0,
            _ => 20.0,
        };
        // Zeichnungen: feste Fläche, standardmäßig ganz ohne Hintergrund
        let draw = matches!(kind, Kind::Draw { .. });
        Self {
            id,
            name,
            kind,
            visible: true,
            pos: [100.0 + (id % 8) as f32 * 30.0, 100.0 + (id % 8) as f32 * 30.0],
            size: if draw { [320.0, 220.0] } else { [250.0, 80.0] },
            auto_size: !draw,
            font_size,
            monospace: false,
            text_color: [255, 255, 255, 255],
            bg_color: [20, 20, 30, if draw { 0 } else { 170 }],
            border_color: [255, 255, 255, 40],
            border_width: if draw { 0.0 } else { 1.0 },
            rounding: 12.0,
            padding: 14.0,
            align: Align::Center,
            opacity: 1.0,
            text_shadow: false,
            move_requested: false,
        }
    }
}

impl Default for WidgetCfg {
    fn default() -> Self {
        Self::new(0, Kind::Text { text: String::new() })
    }
}

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Config {
    next_id: u64,
    widgets: Vec<WidgetCfg>,
}

#[derive(Default, Clone, Copy)]
struct SysStats {
    cpu: f32,
    mem_used: u64,
    mem_total: u64,
}

/// Liest CPU- und RAM-Auslastung nur bei Bedarf (höchstens einmal pro
/// Sekunde). Ohne System-Widget entsteht dadurch gar keine Arbeit, und es
/// braucht keinen eigenen Thread.
struct StatsSampler {
    last: Option<Instant>,
    stats: SysStats,
    /// (Leerlauf, Gesamt) der letzten Messung in 100-ns-Einheiten.
    #[cfg(windows)]
    prev: (u64, u64),
    #[cfg(not(windows))]
    sys: sysinfo::System,
}

impl StatsSampler {
    fn new() -> Self {
        Self {
            last: None,
            stats: SysStats::default(),
            #[cfg(windows)]
            prev: cpu_times(),
            #[cfg(not(windows))]
            sys: sysinfo::System::new(),
        }
    }

    fn get(&mut self) -> SysStats {
        let now = Instant::now();
        if self.last.is_none_or(|t| now.duration_since(t) >= Duration::from_secs(1)) {
            self.last = Some(now);
            self.refresh();
        }
        self.stats
    }

    #[cfg(windows)]
    fn refresh(&mut self) {
        use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

        let (idle, total) = cpu_times();
        let d_idle = idle.saturating_sub(self.prev.0);
        let d_total = total.saturating_sub(self.prev.1);
        if d_total > 0 {
            self.stats.cpu = (100.0 * (1.0 - d_idle as f64 / d_total as f64)).clamp(0.0, 100.0) as f32;
        }
        self.prev = (idle, total);

        // SAFETY: MEMORYSTATUSEX ist eine reine Datenstruktur; dwLength wird wie verlangt gesetzt.
        unsafe {
            let mut mem: MEMORYSTATUSEX = std::mem::zeroed();
            mem.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
            if GlobalMemoryStatusEx(&mut mem) != 0 {
                self.stats.mem_total = mem.ullTotalPhys;
                self.stats.mem_used = mem.ullTotalPhys.saturating_sub(mem.ullAvailPhys);
            }
        }
    }

    #[cfg(not(windows))]
    fn refresh(&mut self) {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        self.stats = SysStats {
            cpu: self.sys.global_cpu_usage(),
            mem_used: self.sys.used_memory(),
            mem_total: self.sys.total_memory(),
        };
    }
}

/// Systemweite CPU-Zeiten als (Leerlauf, Gesamt).
#[cfg(windows)]
fn cpu_times() -> (u64, u64) {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::GetSystemTimes;

    let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut idle, mut kernel, mut user) = (zero, zero, zero);
    // SAFETY: alle drei Zeiger zeigen auf gültige, beschreibbare FILETIMEs.
    unsafe {
        GetSystemTimes(&mut idle, &mut kernel, &mut user);
    }
    let ticks = |f: FILETIME| ((f.dwHighDateTime as u64) << 32) | f.dwLowDateTime as u64;
    // Die Kernel-Zeit enthält die Leerlaufzeit bereits.
    (ticks(idle), ticks(kernel) + ticks(user))
}

/// Gibt selten genutzte Speicherseiten (Start, Editor) an Windows zurück.
/// Was wieder gebraucht wird, lädt Windows von selbst nach.
#[cfg(windows)]
fn trim_memory() {
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, SetProcessWorkingSetSize};
    // SAFETY: Pseudo-Handle des eigenen Prozesses; (MAX, MAX) bedeutet "Arbeitssatz leeren".
    unsafe {
        SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
    }
}

#[cfg(not(windows))]
fn trim_memory() {}

/// Zustand, der zwischen Editor und Widget-Fenstern geteilt wird.
struct Shared {
    cfg: Config,
    edit_mode: bool,
    dirty: bool,
    /// Widget-Fenster sollen sofort neu zeichnen (Änderung im Editor).
    notify: bool,
    stats: StatsSampler,
}

impl Shared {
    /// Änderung aus dem Editor: speichern und Widgets neu zeichnen.
    fn touch(&mut self) {
        self.dirty = true;
        self.notify = true;
    }
}

fn config_path() -> &'static PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("desktop-widgets");
        let _ = std::fs::create_dir_all(&dir);
        dir.join("widgets.json")
    })
}

fn load_config() -> Config {
    let path = config_path();
    let Ok(text) = std::fs::read_to_string(path) else { return Config::default() };
    let mut cfg: Config = match serde_json::from_str(&text) {
        Ok(cfg) => cfg,
        Err(_) => {
            // Unlesbare Datei beiseitelegen, statt sie beim nächsten Speichern
            // mit den Standard-Widgets zu überschreiben
            let _ = std::fs::rename(path, path.with_extension("json.defekt"));
            return Config::default();
        }
    };
    // Doppelte IDs würden zwei Widgets auf ein Fenster legen
    let mut seen = std::collections::HashSet::new();
    let mut max_id = cfg.widgets.iter().map(|w| w.id).max().unwrap_or(0);
    for w in &mut cfg.widgets {
        if !seen.insert(w.id) {
            max_id += 1;
            w.id = max_id;
        }
        w.opacity = if w.opacity.is_finite() { w.opacity.clamp(MIN_OPACITY, 1.0) } else { 1.0 };
    }
    cfg.next_id = cfg.next_id.max(max_id);
    cfg
}

/// Speichert kompakt (ohne Zwischen-String) in eine temporäre Datei und
/// ersetzt dann die alte. Ein Absturz beim Schreiben zerstört so nichts.
fn save_config(cfg: &Config) {
    use std::io::Write as _;
    let path = config_path();
    let tmp = path.with_extension("json.tmp");
    let written = std::fs::File::create(&tmp).is_ok_and(|f| {
        let mut f = std::io::BufWriter::new(f);
        serde_json::to_writer(&mut f, cfg).is_ok() && f.flush().is_ok()
    });
    if written {
        let _ = std::fs::rename(&tmp, path);
    }
}

fn c32(c: [u8; 4]) -> Color32 {
    Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
}

fn color_button(ui: &mut egui::Ui, c: &mut [u8; 4]) -> bool {
    // Unmultipliziert bearbeiten: so bleibt der Farbton auch bei Deckkraft 0 erhalten
    ui.color_edit_button_srgba_unmultiplied(c).changed()
}

/// Deckkraft (Alpha) einer Farbe als Prozent-Regler.
fn opacity_slider(ui: &mut egui::Ui, c: &mut [u8; 4]) -> bool {
    let mut pct = c[3] as f32 / 255.0 * 100.0;
    let changed = ui
        .add(egui::Slider::new(&mut pct, 0.0..=100.0).fixed_decimals(0).suffix(" %"))
        .on_hover_text("Deckkraft: 0 % = völlig durchsichtig")
        .changed();
    if changed {
        c[3] = (pct / 100.0 * 255.0).round() as u8;
    }
    changed
}

// ---------------------------------------------------------------------------
// Inhalt der Widgets
// ---------------------------------------------------------------------------

fn fmt_time(fmt: &str) -> String {
    let mut s = String::new();
    if write!(s, "{}", Local::now().format_localized(fmt, Locale::de_DE)).is_err() {
        s = "Ungültiges Format".into();
    }
    s
}

fn widget_text(kind: &Kind, stats: SysStats) -> String {
    match kind {
        Kind::Clock { format } | Kind::Date { format } => fmt_time(format),
        Kind::Text { text } => text.clone(),
        Kind::System => {
            let gb = |b: u64| b as f64 / 1024.0 / 1024.0 / 1024.0;
            format!(
                "CPU  {:>5.1} %\nRAM  {:.1} / {:.1} GB",
                stats.cpu,
                gb(stats.mem_used),
                gb(stats.mem_total)
            )
        }
        Kind::Countdown { label, target } => match NaiveDate::parse_from_str(target.trim(), "%Y-%m-%d") {
            Ok(date) => {
                let days = (date - Local::now().date_naive()).num_days();
                match days {
                    0 => format!("{label}\nHeute!"),
                    1 => format!("{label}\nnoch 1 Tag"),
                    d if d > 1 => format!("{label}\nnoch {d} Tage"),
                    d => format!("{label}\nvor {} Tagen", -d),
                }
            }
            Err(_) => "Datum: JJJJ-MM-TT".into(),
        },
        Kind::Draw { .. } => String::new(),
    }
}

/// Griff zum Verschieben einer Zeichnung (der Rest der Fläche zeichnet).
fn handle_rect(rect: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_size(rect.min + Vec2::splat(4.0), Vec2::splat(22.0))
}

/// Auf 0,1 px runden: kürzer in der Konfigurationsdatei, sichtbar kein Unterschied.
fn round1(v: f32) -> f32 {
    (v * 10.0).round() / 10.0
}

/// Ramer-Douglas-Peucker: entfernt Punkte, die weniger als `eps` Pixel von der
/// Verbindung ihrer Nachbarn abweichen. Eine Freihand-Linie schrumpft dadurch
/// meist auf einen Bruchteil ihrer Punkte, ohne anders auszusehen.
fn simplify(points: &[[f32; 2]], eps: f32) -> Vec<[f32; 2]> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0, points.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        let (pa, pb) = (points[a], points[b]);
        let (dx, dy) = (pb[0] - pa[0], pb[1] - pa[1]);
        let len = dx.hypot(dy);
        let (mut far, mut far_i) = (0.0, a);
        for (i, p) in points.iter().enumerate().take(b).skip(a + 1) {
            let d = if len > 0.0 {
                ((p[0] - pa[0]) * dy - (p[1] - pa[1]) * dx).abs() / len
            } else {
                (p[0] - pa[0]).hypot(p[1] - pa[1])
            };
            if d > far {
                (far, far_i) = (d, i);
            }
        }
        if far > eps {
            keep[far_i] = true;
            stack.push((a, far_i));
            stack.push((far_i, b));
        }
    }
    let mut out: Vec<[f32; 2]> = points.iter().zip(keep).filter_map(|(p, k)| k.then_some(*p)).collect();
    out.shrink_to_fit();
    out
}

/// Zeichnen mit der Maus auf `rect`. Gibt zurück, ob sich die Zeichnung geändert hat.
fn draw_input(ui: &mut egui::Ui, rect: egui::Rect, w: &mut WidgetCfg) -> bool {
    let id = egui::Id::new(("draw", w.id));
    let Kind::Draw { lines, pen_color, pen_width } = &mut w.kind else { return false };
    let resp = ui
        .interact(rect, id, Sense::click_and_drag())
        .on_hover_cursor(egui::CursorIcon::Crosshair);
    if resp.drag_stopped() {
        // Fertige Linie ausdünnen. Vor dem Zeiger-Check: beim Loslassen gibt es
        // oft keine Zeigerposition mehr.
        if let Some(line) = lines.last_mut() {
            line.points = simplify(&line.points, 0.5);
        }
        return true;
    }
    let Some(pos) = resp.interact_pointer_pos() else { return false };
    let rel = |p: Pos2| [round1(p.x - rect.min.x), round1(p.y - rect.min.y)];
    let start = |p: Pos2| Line { color: *pen_color, width: *pen_width, points: vec![rel(p)] };
    if resp.clicked() {
        lines.push(start(pos)); // einzelner Punkt
        return true;
    }
    if resp.drag_started() {
        // Ziehen wird erst nach ein paar Pixeln erkannt: am Druckpunkt beginnen
        let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(pos);
        lines.push(start(origin));
    }
    if resp.dragged() {
        if let Some(line) = lines.last_mut() {
            let p = rel(pos);
            let far = line.points.last().is_none_or(|l| (l[0] - p[0]).hypot(l[1] - p[1]) >= 1.5);
            if far {
                line.points.push(p);
                return true;
            }
        }
    }
    false
}

/// Kleinste Fenstergröße: Fenster mit 0 px Breite oder Höhe mag der Treiber nicht.
const MIN_SIZE: f32 = 16.0;
/// Ganz unsichtbar soll ein Widget nicht werden, dafür gibt es "Sichtbar".
const MIN_OPACITY: f32 = 0.05;

/// Setzt den Text eines Widgets und berechnet die benötigte Größe.
/// Zeichnungen haben keinen Text; ihre Größe ergibt sich aus den Linien.
fn layout_widget(ui: &egui::Ui, w: &WidgetCfg, stats: SysStats) -> (Option<Arc<egui::Galley>>, Vec2) {
    if let Kind::Draw { lines, .. } = &w.kind {
        let mut max = Vec2::splat(40.0);
        for l in lines {
            for p in &l.points {
                max = max.max(Vec2::new(p[0], p[1]) + Vec2::splat(l.width / 2.0 + w.padding));
            }
        }
        return (None, max);
    }
    let family = if w.monospace { FontFamily::Monospace } else { FontFamily::Proportional };
    let text = widget_text(&w.kind, stats);
    // Nicht simple_singleline: das zeichnet "\n" als Kästchen statt umzubrechen
    let mut job = egui::text::LayoutJob::simple(text, FontId::new(w.font_size, family), c32(w.text_color), f32::INFINITY);
    job.halign = match w.align {
        Align::Left => egui::Align::LEFT,
        Align::Center => egui::Align::Center,
        Align::Right => egui::Align::RIGHT,
    };
    let galley = ui.fonts(|f| f.layout_job(job));
    let needed = (galley.size() + Vec2::splat(w.padding * 2.0)).max(Vec2::splat(MIN_SIZE));
    (Some(galley), needed)
}

/// Zeichnet ein Widget auf die ganze Fläche von `ui`.
fn paint_widget(ui: &egui::Ui, w: &WidgetCfg, galley: Option<Arc<egui::Galley>>, edit_mode: bool) {
    let rect = ui.max_rect();
    // Die Deckkraft gilt für den Inhalt, nicht für die Markierung beim Bearbeiten
    let mut painter = ui.painter().clone();
    painter.multiply_opacity(w.opacity);

    let r = egui::CornerRadius::from(w.rounding);
    painter.rect_filled(rect, r, c32(w.bg_color));
    if w.border_width > 0.0 {
        painter.rect_stroke(
            rect.shrink(w.border_width / 2.0),
            r,
            egui::Stroke::new(w.border_width, c32(w.border_color)),
            egui::StrokeKind::Inside,
        );
    }
    if let Kind::Draw { lines, .. } = &w.kind {
        let clipped = painter.with_clip_rect(rect);
        for l in lines {
            let color = c32(l.color);
            let pts: Vec<Pos2> = l.points.iter().map(|p| rect.min + Vec2::new(p[0], p[1])).collect();
            // Runde Enden (und der einzelne Punkt bei einem Klick)
            for end in [pts.first(), pts.last()].into_iter().flatten() {
                clipped.circle_filled(*end, l.width / 2.0, color);
            }
            if pts.len() > 1 {
                clipped.add(egui::Shape::line(pts, egui::Stroke::new(l.width, color)));
            }
        }
    }
    if let Some(galley) = galley {
        let inner = rect.shrink(w.padding);
        let (x, anchor_x) = match w.align {
            Align::Left => (inner.left(), 0.0),
            Align::Center => (inner.center().x, 0.5),
            Align::Right => (inner.right(), 1.0),
        };
        // Galley-Ursprung liegt bei halign=Center/Right in der Mitte/rechts des Textblocks
        let block_left = x - galley.size().x * anchor_x;
        let origin = Pos2::new(block_left - galley.rect.min.x, inner.center().y - galley.size().y / 2.0);
        if w.text_shadow {
            let offset = Vec2::splat((w.font_size / 24.0).clamp(1.0, 4.0));
            let shadow = Color32::from_black_alpha((w.text_color[3] as f32 * 0.7) as u8);
            painter.galley_with_override_text_color(origin + offset, galley.clone(), shadow);
        }
        painter.galley(origin, galley, Color32::WHITE);
    }
    if edit_mode {
        let painter = ui.painter();
        painter.rect_stroke(rect.shrink(1.0), r, egui::Stroke::new(2.0_f32, ACCENT), egui::StrokeKind::Inside);
        if matches!(w.kind, Kind::Draw { .. }) {
            // Griff zum Verschieben: Kreuz auf blauem Feld
            let h = handle_rect(rect);
            painter.rect_filled(h, 5.0, ACCENT);
            let s = egui::Stroke::new(2.0_f32, Color32::WHITE);
            painter.line_segment([h.center() - Vec2::new(6.0, 0.0), h.center() + Vec2::new(6.0, 0.0)], s);
            painter.line_segment([h.center() - Vec2::new(0.0, 6.0), h.center() + Vec2::new(0.0, 6.0)], s);
        }
    }
}

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

struct App {
    shared: Arc<Mutex<Shared>>,
    selected: Option<u64>,
    /// Editor wurde per X versteckt: dann reicht ein sehr seltenes Neuzeichnen.
    hidden: bool,
    /// Startposition/-größe pro offenem Widget-Fenster (bleibt fix, damit
    /// egui das Fenster nicht bei jedem Frame zurückschiebt).
    spawned: HashMap<u64, ([f32; 2], [f32; 2])>,
    last_save: Instant,
    /// Gesetzt durch "Beenden": Schließen beendet dann wirklich die App.
    quit: bool,
    /// Widget, für das "Löschen" einmal geklickt wurde (wartet auf Bestätigung).
    confirm_delete: Option<u64>,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_fonts(&cc.egui_ctx);
        install_theme(&cc.egui_ctx);

        let mut cfg = load_config();
        if cfg.widgets.is_empty() {
            cfg.next_id = 3;
            cfg.widgets.push(WidgetCfg::new(1, Kind::Clock { format: "%H:%M".into() }));
            let mut d = WidgetCfg::new(2, Kind::Date { format: "%A, %d. %B %Y".into() });
            d.pos = [100.0, 220.0];
            cfg.widgets.push(d);
        }

        // Erstes Widget vorauswählen, damit der Editor nicht leer startet
        let selected = cfg.widgets.first().map(|w| w.id);

        Self {
            shared: Arc::new(Mutex::new(Shared {
                cfg,
                edit_mode: true,
                dirty: true,
                notify: false,
                stats: StatsSampler::new(),
            })),
            selected,
            hidden: false,
            spawned: HashMap::new(),
            last_save: Instant::now(),
            quit: false,
            confirm_delete: None,
        }
    }

    fn add(&mut self, kind: Kind) {
        let mut sh = self.shared.lock().unwrap();
        sh.cfg.next_id += 1;
        let id = sh.cfg.next_id;
        sh.cfg.widgets.push(WidgetCfg::new(id, kind));
        sh.touch();
        self.selected = Some(id);
    }

    fn show_widget_windows(&mut self, ctx: &egui::Context) {
        let (widgets, edit_mode, notify): (Vec<(u64, [f32; 2], [f32; 2])>, bool, bool) = {
            let mut sh = self.shared.lock().unwrap();
            (
                sh.cfg
                    .widgets
                    .iter()
                    .filter(|w| w.visible)
                    .map(|w| (w.id, w.pos, w.size))
                    .collect(),
                sh.edit_mode,
                std::mem::take(&mut sh.notify),
            )
        };
        self.spawned.retain(|id, _| {
            let keep = widgets.iter().any(|w| w.0 == *id);
            if !keep {
                // Das Fenster wird geschlossen. Ein wieder eingeblendetes Widget
                // bekommt ein neues Fenster, das die Transparenz neu braucht –
                // auch wenn Windows dasselbe Handle wiederverwendet.
                ctx.data_mut(|d| d.remove::<(Instant, bool, isize)>(z_key(*id)));
            }
            keep
        });

        for (id, pos, size) in widgets {
            let (spawn_pos, spawn_size) = *self.spawned.entry(id).or_insert((pos, size));
            let builder = egui::ViewportBuilder::default()
                // Eindeutiger Titel, damit das Fenster per FindWindowW gefunden wird
                .with_title(widget_title(id))
                .with_position(spawn_pos)
                .with_inner_size(spawn_size)
                .with_decorations(false)
                .with_transparent(true)
                .with_resizable(false)
                .with_taskbar(false)
                // Gesperrt: Klicks gehen durch das Widget auf den Desktop
                .with_mouse_passthrough(!edit_mode);

            let shared = self.shared.clone();
            let viewport_id = egui::ViewportId::from_hash_of(("widget", id));
            ctx.show_viewport_deferred(viewport_id, builder, move |ctx, _class| {
                widget_viewport(ctx, id, &shared)
            });
            // Widgets zeichnen sonst nur im Sekundentakt: Änderungen sofort zeigen
            if notify {
                ctx.request_repaint_of(viewport_id);
            }
        }
    }
}

fn widget_title(id: u64) -> String {
    format!("desktop-widget-{id}")
}

/// Schlüssel in `ctx.data` für den Desktop-Zustand eines Widget-Fensters.
/// Pro Widget, denn `ctx.data` wird von allen Fenstern geteilt.
fn z_key(id: u64) -> egui::Id {
    egui::Id::new(("desktop_z", id))
}

/// Macht das Fenster zu einem "Besitz" des Desktop-Fensters (Progman), damit
/// es bei Win+D ("Desktop anzeigen") sichtbar bleibt. Mit `on_top` liegt es
/// über allen Fenstern (Bearbeitungsmodus, damit man es greifen kann), sonst
/// ganz hinten in der Z-Reihenfolge, also hinter allen App-Fenstern und direkt
/// über dem Desktop.
///
/// Gibt das Fensterhandle zurück, sobald das Fenster existiert.
#[cfg(windows)]
fn attach_to_desktop(title: &str, on_top: bool) -> Option<isize> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        FindWindowW, SetWindowLongPtrW, SetWindowPos, GWLP_HWNDPARENT, HWND_BOTTOM, HWND_TOPMOST,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSENDCHANGING, SWP_NOSIZE,
    };
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let class = wide("Progman");
    let title = wide(title);
    unsafe {
        let progman = FindWindowW(class.as_ptr(), std::ptr::null());
        let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
        if progman.is_null() || hwnd.is_null() {
            return None;
        }
        SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, progman as isize);
        // HWND_BOTTOM nimmt dem Fenster auch wieder den Topmost-Status
        SetWindowPos(
            hwnd,
            if on_top { HWND_TOPMOST } else { HWND_BOTTOM },
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_NOSENDCHANGING,
        );
        Some(hwnd as isize)
    }
}

#[cfg(not(windows))]
fn attach_to_desktop(_title: &str, _on_top: bool) -> Option<isize> {
    Some(0)
}

/// Schaltet die Alpha-Transparenz des Fensters ein.
///
/// `with_transparent(true)` reicht unter Windows nicht: glutin-winit nimmt
/// Widget-Fenstern die Transparenz wieder weg, wenn der Treiber kein
/// `WGL_TRANSPARENT_ARB` meldet (z. B. Intel Arc). Das Pixelformat hat aber
/// trotzdem einen Alphakanal. Darum hier derselbe DWM-Aufruf, den winit sonst
/// selbst macht: "Blur behind" mit leerer Region = Alpha wird durchgereicht.
#[cfg(windows)]
fn enable_alpha(hwnd: isize) {
    use windows_sys::Win32::Graphics::Dwm::{
        DwmEnableBlurBehindWindow, DWM_BB_BLURREGION, DWM_BB_ENABLE, DWM_BLURBEHIND,
    };
    use windows_sys::Win32::Graphics::Gdi::{CreateRectRgn, DeleteObject};
    // SAFETY: hwnd stammt aus FindWindowW; die Region wird nach dem Aufruf freigegeben.
    unsafe {
        let region = CreateRectRgn(0, 0, -1, -1);
        let bb = DWM_BLURBEHIND {
            dwFlags: DWM_BB_ENABLE | DWM_BB_BLURREGION,
            fEnable: 1,
            hRgnBlur: region,
            fTransitionOnMaximized: 0,
        };
        DwmEnableBlurBehindWindow(hwnd as _, &bb);
        DeleteObject(region);
    }
}

#[cfg(not(windows))]
fn enable_alpha(_hwnd: isize) {}

fn widget_viewport(ctx: &egui::Context, id: u64, shared: &Arc<Mutex<Shared>>) {
    // Regelmäßig an den Desktop hängen. Gesperrt ganz nach hinten, damit ein
    // Widget nie über einem App-Fenster liegt; im Bearbeitungsmodus ganz nach
    // vorne, damit es sich greifen und ziehen lässt. Ein Moduswechsel wirkt sofort.
    let on_top = shared.lock().unwrap().edit_mode;
    let key = z_key(id);
    let now = Instant::now();
    let last = ctx.data(|d| d.get_temp::<(Instant, bool, isize)>(key));
    let due = last.is_none_or(|(t, top, _)| top != on_top || now.duration_since(t) >= Duration::from_millis(400));
    if due {
        if let Some(hwnd) = attach_to_desktop(&widget_title(id), on_top) {
            // Einmal pro Fenster (ein wieder eingeblendetes Widget hat ein neues)
            if last.is_none_or(|(_, _, old)| old != hwnd) {
                enable_alpha(hwnd);
                ctx.request_repaint();
            }
            ctx.data_mut(|d| d.insert_temp(key, (now, on_top, hwnd)));
        }
    }

    let (outer, inner) = ctx.input(|i| (i.viewport().outer_rect, i.viewport().inner_rect));
    // Im Bearbeitungsmodus öfter zeichnen, damit Verschieben flüssig erfasst wird
    let mut next_repaint = Duration::from_millis(500);

    egui::CentralPanel::default()
        .frame(egui::Frame::new())
        .show(ctx, |ui| {
            let mut guard = shared.lock().unwrap();
            let sh = &mut *guard;
            let edit_mode = sh.edit_mode;
            let Some(w) = sh.cfg.widgets.iter_mut().find(|w| w.id == id) else { return };
            let stats = if matches!(w.kind, Kind::System) { sh.stats.get() } else { SysStats::default() };
            if !edit_mode {
                // Gesperrt: genau zum nächsten Sekundenwechsel wieder zeichnen
                let ms = Local::now().timestamp_subsec_millis().min(999) as u64;
                next_repaint = Duration::from_millis(1005 - ms);
            }

            let (galley, needed) = layout_widget(ui, w, stats);
            paint_widget(ui, w, galley, edit_mode);

            // Größe
            let want = if w.auto_size { needed } else { Vec2::from(w.size) };
            if let Some(inner) = inner {
                if (inner.size() - want).length() > 1.0 {
                    ctx.send_viewport_cmd(ViewportCommand::InnerSize(want));
                }
            }
            let mut changed = false;
            if w.auto_size && Vec2::from(w.size) != want {
                w.size = want.into();
                changed = true;
            }

            // Position
            if w.move_requested {
                ctx.send_viewport_cmd(ViewportCommand::OuterPosition(Pos2::from(w.pos)));
                w.move_requested = false;
            } else if edit_mode {
                // Zeichnungen: die Fläche zeichnet, nur der Griff verschiebt.
                // Der Griff kommt zuletzt und liegt damit über der Zeichenfläche.
                let full = ui.max_rect();
                let is_draw = matches!(w.kind, Kind::Draw { .. });
                if is_draw && draw_input(ui, full, w) {
                    changed = true;
                    ctx.request_repaint();
                    // Vorschau im Editor mitziehen
                    ctx.request_repaint_of(egui::ViewportId::ROOT);
                }
                let drag_rect = if is_draw { handle_rect(full) } else { full };
                let resp = ui
                    .interact(drag_rect, egui::Id::new("drag"), Sense::click_and_drag())
                    .on_hover_cursor(egui::CursorIcon::Grab);
                if resp.drag_started() {
                    ctx.send_viewport_cmd(ViewportCommand::StartDrag);
                }
                if let Some(outer) = outer {
                    let p = [outer.min.x.round(), outer.min.y.round()];
                    if p != w.pos {
                        w.pos = p;
                        changed = true;
                    }
                }
            }
            if changed {
                sh.dirty = true;
            }
        });

    ctx.request_repaint_after(next_repaint);
}

const ACCENT: Color32 = Color32::from_rgb(80, 160, 255);
const BG: Color32 = Color32::from_rgb(19, 21, 27);
const PANEL: Color32 = Color32::from_rgb(25, 28, 36);
const CARD: Color32 = Color32::from_rgb(33, 37, 47);
const LINE: Color32 = Color32::from_rgb(48, 53, 66);
const TEXT: Color32 = Color32::from_rgb(226, 229, 236);
const TEXT_DIM: Color32 = Color32::from_rgb(140, 147, 163);
const DANGER: Color32 = Color32::from_rgb(240, 110, 110);

fn install_theme(ctx: &egui::Context) {
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.all_styles_mut(|s| {
        use egui::TextStyle::{Body, Button, Heading, Monospace, Small};
        s.text_styles.insert(Heading, FontId::proportional(19.0));
        s.text_styles.insert(Body, FontId::proportional(14.5));
        s.text_styles.insert(Button, FontId::proportional(14.5));
        s.text_styles.insert(Small, FontId::proportional(12.0));
        s.text_styles.insert(Monospace, FontId::monospace(13.5));

        s.spacing.item_spacing = Vec2::new(8.0, 8.0);
        s.spacing.button_padding = Vec2::new(10.0, 5.0);
        s.spacing.interact_size.y = 28.0;
        s.spacing.slider_width = 180.0;

        let v = &mut s.visuals;
        v.panel_fill = BG;
        v.window_fill = PANEL;
        v.extreme_bg_color = Color32::from_rgb(14, 15, 20);
        v.faint_bg_color = CARD;
        v.selection.bg_fill = ACCENT.gamma_multiply(0.35);
        v.selection.stroke = egui::Stroke::new(1.0_f32, ACCENT);
        v.window_stroke = egui::Stroke::new(1.0_f32, LINE);

        let r = egui::CornerRadius::same(6);
        v.window_corner_radius = egui::CornerRadius::same(8);
        v.menu_corner_radius = egui::CornerRadius::same(8);
        v.widgets.noninteractive.corner_radius = r;
        v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, LINE);
        for (w, fill) in [
            (&mut v.widgets.inactive, Color32::from_rgb(44, 49, 62)),
            (&mut v.widgets.hovered, Color32::from_rgb(56, 62, 78)),
            (&mut v.widgets.active, Color32::from_rgb(66, 73, 92)),
            (&mut v.widgets.open, Color32::from_rgb(56, 62, 78)),
        ] {
            w.corner_radius = r;
            w.bg_fill = fill;
            w.weak_bg_fill = fill;
            w.bg_stroke = egui::Stroke::NONE;
            w.expansion = 0.0;
            w.fg_stroke.color = TEXT;
        }
        v.widgets.noninteractive.fg_stroke.color = TEXT;
    });
}

fn install_fonts(ctx: &egui::Context) {
    // Segoe UI verwenden, falls vorhanden (schöner als die Standardschrift)
    let mut fonts = egui::FontDefinitions::default();
    let win = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into());
    if let Ok(bytes) = std::fs::read(format!("{win}\\Fonts\\segoeui.ttf")) {
        fonts
            .font_data
            .insert("segoe".into(), Arc::new(egui::FontData::from_owned(bytes)));
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, "segoe".into());
    }
    ctx.set_fonts(fonts);
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4] // nötig für transparente Widget-Fenster
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Editor schließen (X) versteckt nur das Fenster, die Widgets laufen weiter.
        // Erneutes Starten der App holt den Editor zurück (siehe main).
        if ctx.input(|i| i.viewport().close_requested()) && !self.quit {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            // Ein erneuter Start zeigt das Fenster per ShowWindow an winit vorbei.
            // winit hält es dann weiter für versteckt und ignoriert Visible(false).
            // Darum zuerst den Zustand angleichen, dann verstecken.
            ctx.send_viewport_cmd(ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            let mut sh = self.shared.lock().unwrap();
            sh.edit_mode = false;
            sh.notify = true;
            save_config(&sh.cfg);
            sh.dirty = false;
            self.hidden = true;
            trim_memory();
        } else if self.hidden && ctx.input(|i| i.viewport().focused == Some(true)) {
            // Ein erneuter Start der App hat den Editor zurückgeholt
            self.hidden = false;
        }

        // Versteckter Editor: keine Oberfläche aufbauen, spart Arbeit und Speicher
        if !self.hidden {
            self.editor_ui(ctx);
        }
        self.show_widget_windows(ctx);

        // Automatisch speichern
        if self.last_save.elapsed() > Duration::from_secs(2) {
            let mut sh = self.shared.lock().unwrap();
            if sh.dirty {
                save_config(&sh.cfg);
                sh.dirty = false;
            }
            self.last_save = Instant::now();
        }
        // Versteckt gibt es nichts zu sehen und nichts zu speichern
        let wait = if self.hidden { 5000 } else { 500 };
        ctx.request_repaint_after(Duration::from_millis(wait));
    }

    /// Läuft vor jedem Frame jedes Fensters.
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        // egui legt den Schrift-Atlas so breit an, wie die Grafikkarte erlaubt
        // (bis 16384 px). Jede Zeile kostet dann 64 KB im RAM plus dieselbe
        // Menge als Textur – bei großen Uhren-Schriften schnell zig MB.
        // 2048 px reichen für jede Schriftgröße hier. Muss für alle Fenster
        // gleich sein, sonst baut egui den Atlas in jedem Frame neu.
        raw_input.max_texture_side = raw_input.max_texture_side.map(|s| s.min(2048));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        save_config(&self.shared.lock().unwrap().cfg);
    }
}

// ---------------------------------------------------------------------------
// Editor-Oberfläche
// ---------------------------------------------------------------------------

/// Überschrift plus Karte mit zweispaltigem Raster für Eigenschaften.
fn section(ui: &mut egui::Ui, title: &str, add_contents: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(6.0);
    ui.label(egui::RichText::new(title).small().strong().color(TEXT_DIM));
    egui::Frame::new()
        .fill(CARD)
        .corner_radius(8.0)
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::Grid::new(title)
                .num_columns(2)
                .min_col_width(100.0)
                .spacing([14.0, 10.0])
                .show(ui, add_contents);
        });
}

fn dim(text: &str) -> egui::RichText {
    egui::RichText::new(text).small().color(TEXT_DIM)
}

impl App {
    fn editor_ui(&mut self, ctx: &egui::Context) {
        self.top_bar(ctx);
        self.status_bar(ctx);
        self.widget_list(ctx);
        self.detail_panel(ctx);
    }

    fn top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top")
            .frame(egui::Frame::new().fill(PANEL).inner_margin(egui::Margin::symmetric(14, 10)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.heading("Desktop Widgets");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button("Beenden")
                            .on_hover_text("Beendet die App samt Widgets. Das X versteckt nur den Editor.")
                            .clicked()
                        {
                            self.quit = true;
                            ctx.send_viewport_cmd(ViewportCommand::Close);
                        }
                        let mut sh = self.shared.lock().unwrap();
                        let mut edit = sh.edit_mode;
                        let label = if edit { "Bearbeiten: an" } else { "Bearbeiten: aus" };
                        if ui
                            .toggle_value(&mut edit, label)
                            .on_hover_text("Aus = Widgets sind fixiert und Klicks gehen auf den Desktop durch")
                            .changed()
                        {
                            sh.edit_mode = edit;
                            sh.notify = true;
                        }
                    });
                });
            });
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("status")
            .frame(egui::Frame::new().fill(PANEL).inner_margin(egui::Margin::symmetric(14, 6)))
            .show(ctx, |ui| {
                let (edit_mode, dirty) = {
                    let sh = self.shared.lock().unwrap();
                    (sh.edit_mode, sh.dirty)
                };
                ui.horizontal(|ui| {
                    ui.label(dim(if edit_mode {
                        "Widgets liegen über allen Fenstern und lassen sich mit der Maus verschieben."
                    } else {
                        "Widgets sind fixiert, Klicks gehen auf den Desktop durch."
                    }));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(dim(if dirty { "Speichert …" } else { "Gespeichert" }));
                    });
                });
            });
    }

    fn widget_list(&mut self, ctx: &egui::Context) {
        let mut new_kind = None;
        egui::SidePanel::left("list")
            .resizable(false)
            .exact_width(230.0)
            .frame(egui::Frame::new().fill(PANEL).inner_margin(egui::Margin::same(10)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("WIDGETS").small().strong().color(TEXT_DIM));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.menu_button("+ Neu", |ui| {
                            let in_30_days = Local::now().date_naive() + chrono::Days::new(30);
                            for (label, kind) in [
                                ("Uhr", Kind::Clock { format: "%H:%M:%S".into() }),
                                ("Datum", Kind::Date { format: "%A, %d. %B %Y".into() }),
                                ("Text", Kind::Text { text: "Hallo Desktop!".into() }),
                                ("System", Kind::System),
                                (
                                    "Countdown",
                                    Kind::Countdown {
                                        label: "Urlaub".into(),
                                        target: in_30_days.format("%Y-%m-%d").to_string(),
                                    },
                                ),
                                (
                                    "Zeichnung",
                                    Kind::Draw { lines: Vec::new(), pen_color: [255, 255, 255, 255], pen_width: 4.0 },
                                ),
                            ] {
                                if ui.button(label).clicked() {
                                    new_kind = Some(kind);
                                    ui.close_menu();
                                }
                            }
                        });
                    });
                });

                let mut sh = self.shared.lock().unwrap();
                let mut changed = false;
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    if sh.cfg.widgets.is_empty() {
                        ui.label(dim("Noch keine Widgets."));
                    }
                    for w in &mut sh.cfg.widgets {
                        let selected = self.selected == Some(w.id);
                        let (rect, resp) =
                            ui.allocate_exact_size(Vec2::new(ui.available_width(), 46.0), Sense::click());
                        let fill = if selected {
                            ACCENT.gamma_multiply(0.22)
                        } else if resp.hovered() {
                            CARD
                        } else {
                            Color32::TRANSPARENT
                        };
                        ui.painter().rect_filled(rect, 6.0, fill);
                        if selected {
                            let bar = egui::Rect::from_min_size(
                                rect.left_top() + Vec2::new(0.0, 10.0),
                                Vec2::new(3.0, rect.height() - 20.0),
                            );
                            ui.painter().rect_filled(bar, 2.0, ACCENT);
                        }
                        // Lange Namen am Zeilenrand abschneiden
                        let text = ui.painter().with_clip_rect(rect.shrink2(Vec2::new(8.0, 0.0)));
                        let x = rect.left() + 42.0;
                        text.text(
                            Pos2::new(x, rect.top() + 6.0),
                            egui::Align2::LEFT_TOP,
                            &w.name,
                            FontId::proportional(14.5),
                            if w.visible { TEXT } else { TEXT_DIM },
                        );
                        text.text(
                            Pos2::new(x, rect.top() + 26.0),
                            egui::Align2::LEFT_TOP,
                            w.kind.label(),
                            FontId::proportional(12.0),
                            TEXT_DIM,
                        );
                        let checkbox = egui::Rect::from_center_size(
                            Pos2::new(rect.left() + 22.0, rect.center().y),
                            Vec2::splat(20.0),
                        );
                        changed |= ui
                            .put(checkbox, egui::Checkbox::without_text(&mut w.visible))
                            .on_hover_text("Auf dem Desktop anzeigen")
                            .changed();
                        if resp.clicked() {
                            self.selected = Some(w.id);
                        }
                    }
                });
                if changed {
                    sh.touch();
                }
            });
        if let Some(kind) = new_kind {
            self.add(kind);
        }
    }

    fn detail_panel(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(BG).inner_margin(egui::Margin::same(16)))
            .show(ctx, |ui| {
                let mut guard = self.shared.lock().unwrap();
                let sh = &mut *guard;
                let edit_mode = sh.edit_mode;
                let Some(idx) = self
                    .selected
                    .and_then(|id| sh.cfg.widgets.iter().position(|w| w.id == id))
                else {
                    ui.centered_and_justified(|ui| {
                        ui.label(
                            egui::RichText::new("Links ein Widget auswählen\noder mit „+ Neu“ eines anlegen.")
                                .color(TEXT_DIM),
                        );
                    });
                    return;
                };

                let stats = if matches!(sh.cfg.widgets[idx].kind, Kind::System) {
                    sh.stats.get()
                } else {
                    SysStats::default()
                };
                let mut delete = false;
                let mut duplicate = false;
                let widget_id = sh.cfg.widgets[idx].id;
                let mut confirm_delete = self.confirm_delete == Some(widget_id);
                let mut dirty = false;
                {
                    let w = &mut sh.cfg.widgets[idx];
                    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        // Kopfzeile: Name links, Aktionen rechts
                        ui.horizontal(|ui| {
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                // Löschen lässt sich nicht rückgängig machen: zweiter Klick bestätigt
                                if confirm_delete {
                                    delete = ui.button(egui::RichText::new("Wirklich löschen").color(DANGER)).clicked();
                                    if ui.button("Abbrechen").clicked() {
                                        confirm_delete = false;
                                    }
                                } else {
                                    confirm_delete = ui.button(egui::RichText::new("Löschen").color(DANGER)).clicked();
                                    duplicate = ui.button("Duplizieren").clicked();
                                }
                                dirty |= ui
                                    .add(
                                        egui::TextEdit::singleline(&mut w.name)
                                            .font(egui::TextStyle::Heading)
                                            .hint_text("Name")
                                            .desired_width(f32::INFINITY),
                                    )
                                    .changed();
                            });
                        });

                        // Vorschau auf einer "Desktop"-Fläche. Die Größe selbst berechnen:
                        // w.size hinkt bei automatischer Größe hinterher (oder steht bei
                        // ausgeblendeten Widgets ganz still).
                        let avail = ui.available_width();
                        let (galley, needed) = layout_widget(ui, w, stats);
                        let full = if w.auto_size { needed } else { Vec2::from(w.size) };
                        let size = full.min(Vec2::new(avail - 48.0, 260.0)).max(Vec2::ZERO);
                        let (stage, _) =
                            ui.allocate_exact_size(Vec2::new(avail, size.y + 48.0), Sense::hover());
                        ui.painter().rect_filled(stage, 10.0, Color32::from_rgb(52, 78, 110));
                        let rect = egui::Rect::from_center_size(stage.center(), size);
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                        child.set_clip_rect(rect);
                        paint_widget(&child, w, galley, false);
                        // Zeichnungen lassen sich auch direkt in der Vorschau malen
                        dirty |= draw_input(&mut child, rect, w);

                        section(ui, "INHALT", |ui| match &mut w.kind {
                            Kind::Clock { format } | Kind::Date { format } => {
                                ui.label("Format");
                                ui.vertical(|ui| {
                                    dirty |= ui.text_edit_singleline(format).changed();
                                    ui.horizontal_wrapped(|ui| {
                                        for p in ["%H:%M", "%H:%M:%S", "%I:%M %p", "%d.%m.%Y", "%A, %d. %B %Y", "%a %d.%m."] {
                                            if ui.small_button(p).clicked() {
                                                *format = p.into();
                                                dirty = true;
                                            }
                                        }
                                    });
                                    ui.label(dim("%H Std, %M Min, %S Sek, %d Tag, %m Monat, %Y Jahr, %A Wochentag, %B Monatsname"));
                                });
                                ui.end_row();
                            }
                            Kind::Text { text } => {
                                ui.label("Text");
                                dirty |= ui.text_edit_multiline(text).changed();
                                ui.end_row();
                            }
                            Kind::Countdown { label, target } => {
                                ui.label("Bezeichnung");
                                dirty |= ui.text_edit_singleline(label).changed();
                                ui.end_row();
                                ui.label("Zieldatum");
                                ui.horizontal(|ui| {
                                    dirty |= ui.text_edit_singleline(target).changed();
                                    ui.label(dim("JJJJ-MM-TT"));
                                });
                                ui.end_row();
                            }
                            Kind::Draw { lines, pen_color, pen_width } => {
                                ui.label("Stift");
                                ui.horizontal(|ui| {
                                    dirty |= color_button(ui, pen_color);
                                    dirty |= ui
                                        .add(egui::Slider::new(pen_width, 1.0..=40.0).step_by(1.0).suffix(" px"))
                                        .changed();
                                });
                                ui.end_row();
                                ui.label("Zeichnung");
                                ui.vertical(|ui| {
                                    ui.horizontal(|ui| {
                                        ui.add_enabled_ui(!lines.is_empty(), |ui| {
                                            if ui.button("Rückgängig").clicked() {
                                                lines.pop();
                                                dirty = true;
                                            }
                                            if ui.button("Leeren").clicked() {
                                                lines.clear();
                                                dirty = true;
                                            }
                                        });
                                    });
                                    ui.label(dim("In der Vorschau oder (Bearbeiten: an) direkt auf dem Widget zeichnen."));
                                    ui.label(dim("Verschieben: am blauen Griff oben links ziehen."));
                                });
                                ui.end_row();
                            }
                            Kind::System => {
                                ui.label("Anzeige");
                                ui.label(dim("CPU- und RAM-Auslastung, jede Sekunde aktualisiert"));
                                ui.end_row();
                            }
                        });

                        // Zeichnungen haben keinen Text
                        let is_draw = matches!(w.kind, Kind::Draw { .. });
                        if !is_draw {
                        section(ui, "SCHRIFT", |ui| {
                            ui.label("Größe");
                            // Ganze Schritte: jede neue Größe belegt Platz im Schrift-Atlas
                            dirty |= ui
                                .add(egui::Slider::new(&mut w.font_size, 8.0..=200.0).step_by(1.0).suffix(" px"))
                                .changed();
                            ui.end_row();

                            ui.label("Schriftart");
                            ui.horizontal(|ui| {
                                dirty |= ui.selectable_value(&mut w.monospace, false, "Normal").changed();
                                dirty |= ui.selectable_value(&mut w.monospace, true, "Monospace").changed();
                            });
                            ui.end_row();

                            ui.label("Ausrichtung");
                            ui.horizontal(|ui| {
                                dirty |= ui.selectable_value(&mut w.align, Align::Left, "Links").changed();
                                dirty |= ui.selectable_value(&mut w.align, Align::Center, "Mitte").changed();
                                dirty |= ui.selectable_value(&mut w.align, Align::Right, "Rechts").changed();
                            });
                            ui.end_row();

                            ui.label("Farbe");
                            dirty |= color_button(ui, &mut w.text_color);
                            ui.end_row();

                            ui.label("Schatten");
                            dirty |= ui
                                .checkbox(&mut w.text_shadow, "Textschatten")
                                .on_hover_text("Hält den Text ohne Hintergrund auf hellen und dunklen Bildern lesbar")
                                .changed();
                            ui.end_row();
                        });
                        }

                        section(ui, "AUSSEHEN", |ui| {
                            ui.label("Deckkraft");
                            let mut pct = w.opacity * 100.0;
                            if ui
                                .add(
                                    egui::Slider::new(&mut pct, MIN_OPACITY * 100.0..=100.0)
                                        .fixed_decimals(0)
                                        .suffix(" %"),
                                )
                                .on_hover_text("Ganzes Widget: Text, Hintergrund, Rahmen und Linien")
                                .changed()
                            {
                                w.opacity = pct / 100.0;
                                dirty = true;
                            }
                            ui.end_row();

                            ui.label("Hintergrund");
                            ui.horizontal(|ui| {
                                dirty |= color_button(ui, &mut w.bg_color);
                                dirty |= opacity_slider(ui, &mut w.bg_color);
                                let bare = w.bg_color[3] == 0 && w.border_width == 0.0;
                                if ui
                                    .add_enabled(!bare, egui::Button::new("Durchsichtig").small())
                                    .on_hover_text("Hintergrund und Rahmen ausblenden, nur der Inhalt bleibt")
                                    .clicked()
                                {
                                    w.bg_color[3] = 0;
                                    w.border_width = 0.0;
                                    dirty = true;
                                }
                            });
                            ui.end_row();

                            ui.label("Rahmen");
                            ui.horizontal(|ui| {
                                dirty |= color_button(ui, &mut w.border_color);
                                dirty |= ui
                                    .add(egui::Slider::new(&mut w.border_width, 0.0..=10.0).suffix(" px"))
                                    .changed();
                            });
                            ui.end_row();

                            ui.label("Eckenradius");
                            dirty |= ui.add(egui::Slider::new(&mut w.rounding, 0.0..=100.0).suffix(" px")).changed();
                            ui.end_row();

                            ui.label("Innenabstand");
                            dirty |= ui.add(egui::Slider::new(&mut w.padding, 0.0..=80.0).suffix(" px")).changed();
                            ui.end_row();
                        });

                        section(ui, "POSITION & GRÖSSE", |ui| {
                            ui.label("Größe");
                            ui.horizontal(|ui| {
                                dirty |= ui.checkbox(&mut w.auto_size, "automatisch").changed();
                                ui.add_enabled_ui(!w.auto_size, |ui| {
                                    dirty |= ui.add(egui::DragValue::new(&mut w.size[0]).range(20.0..=4000.0).prefix("B ")).changed();
                                    dirty |= ui.add(egui::DragValue::new(&mut w.size[1]).range(20.0..=4000.0).prefix("H ")).changed();
                                });
                            });
                            ui.end_row();

                            ui.label("Position");
                            ui.horizontal(|ui| {
                                let mut moved = false;
                                moved |= ui.add(egui::DragValue::new(&mut w.pos[0]).prefix("X ")).changed();
                                moved |= ui.add(egui::DragValue::new(&mut w.pos[1]).prefix("Y ")).changed();
                                if moved {
                                    w.move_requested = true;
                                    dirty = true;
                                }
                                if edit_mode {
                                    ui.label(dim("oder Widget mit der Maus ziehen"));
                                }
                            });
                            ui.end_row();

                            ui.label("Sichtbar");
                            dirty |= ui.checkbox(&mut w.visible, "auf dem Desktop anzeigen").changed();
                            ui.end_row();
                        });
                    });
                }

                if duplicate {
                    sh.cfg.next_id += 1;
                    let mut copy = sh.cfg.widgets[idx].clone();
                    copy.id = sh.cfg.next_id;
                    copy.name = format!("{} (Kopie)", copy.name);
                    copy.pos[0] += 30.0;
                    copy.pos[1] += 30.0;
                    self.selected = Some(copy.id);
                    sh.cfg.widgets.push(copy);
                    dirty = true;
                }
                if delete {
                    sh.cfg.widgets.remove(idx);
                    // Nächstes Widget auswählen, damit der Editor nicht leer wird
                    self.selected = sh.cfg.widgets.get(idx.min(sh.cfg.widgets.len().saturating_sub(1))).map(|w| w.id);
                    confirm_delete = false;
                    dirty = true;
                }
                self.confirm_delete = confirm_delete.then_some(widget_id);
                if dirty {
                    sh.touch();
                }
            });
    }
}

const EDITOR_TITLE: &str = "Desktop Widgets – Editor";

/// Läuft die App schon, wird deren (evtl. versteckter) Editor nach vorne geholt.
#[cfg(windows)]
fn show_running_editor() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{FindWindowW, SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW};
    let title: Vec<u16> = EDITOR_TITLE.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
        if hwnd.is_null() {
            return false;
        }
        ShowWindow(hwnd, SW_SHOW);
        ShowWindow(hwnd, SW_RESTORE);
        SetForegroundWindow(hwnd);
    }
    true
}

#[cfg(not(windows))]
fn show_running_editor() -> bool {
    false
}

fn main() -> eframe::Result {
    if show_running_editor() {
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(EDITOR_TITLE)
            .with_inner_size([820.0, 620.0])
            .with_min_inner_size([600.0, 400.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Desktop Widgets",
        options,
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simplify_straight_line_keeps_ends() {
        let pts: Vec<[f32; 2]> = (0..100).map(|i| [i as f32, 2.0 * i as f32]).collect();
        assert_eq!(simplify(&pts, 0.5), vec![[0.0, 0.0], [99.0, 198.0]]);
    }

    #[test]
    fn simplify_keeps_corner() {
        let mut pts: Vec<[f32; 2]> = (0..=50).map(|i| [i as f32, 0.0]).collect();
        pts.extend((1..=50).map(|i| [50.0, i as f32]));
        assert_eq!(simplify(&pts, 0.5), vec![[0.0, 0.0], [50.0, 0.0], [50.0, 50.0]]);
    }

    #[test]
    fn old_config_without_new_fields_loads() {
        let json = r#"{"next_id":1,"widgets":[{"id":1,"name":"Uhr","kind":{"Clock":{"format":"%H:%M"}},
            "visible":true,"pos":[1.0,2.0],"size":[3.0,4.0],"auto_size":true,"font_size":20.0,
            "monospace":false,"text_color":[1,2,3,4],"bg_color":[0,0,0,0],"border_color":[0,0,0,0],
            "border_width":0.0,"rounding":0.0,"padding":0.0,"align":"Left"}]}"#;
        let cfg: Config = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.widgets[0].opacity, 1.0);
        assert!(!cfg.widgets[0].text_shadow);
    }
}
