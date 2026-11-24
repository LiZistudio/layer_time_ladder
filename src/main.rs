#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::io;
use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, App, Application, Bounds, ClickEvent, Context, FocusHandle, KeyDownEvent,
    MouseButton, SharedString, TitlebarOptions, Window, WindowBounds, WindowOptions, div,
    prelude::*, px, rgb, size,
};

// 界面配色（浅色，参照 Windows 原生对话框）
const WINDOW_BG: u32 = 0xf3f2f1;
const PANEL_BG: u32 = 0xffffff;
const BOX_BG: u32 = 0xffffff;
const BOX_BORDER: u32 = 0xc9c9c9;
const SUBTLE_BG: u32 = 0xf7f7f7;
const TEXT: u32 = 0x1f1f1f;
const MUTED: u32 = 0x6b6b6b;
const ACCENT: u32 = 0x1a73e8;
const ACCENT_HOVER: u32 = 0x1668d0;
const ACCENT_TEXT: u32 = 0xffffff;
const SUCCESS: u32 = 0x0f7b0f;
const DANGER: u32 = 0xc42b1c;

// 默认参数
const DEFAULT_SIDE: f64 = 40.0;
const DEFAULT_WALLS: u32 = 2;
const DEFAULT_LINE_WIDTH: f64 = 0.42;
const DEFAULT_INITIAL_TIME: f64 = 8.0;
const DEFAULT_DECREASE: f64 = 0.5;
const DEFAULT_INTERVAL: u32 = 100;
const MIN_LAYER_TIME: f64 = 1.0;
const MARKER_BUMP: f64 = 0.1; // 速度变化层外壁标记的 X 偏移量 (mm)
const PARAM_ALIGN_HINT: &str =
    "方管参数设置需和实际方管切片设置对齐。方管高度要和步距以及间隔层数对齐。";

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Auto,
    Marker,
}

struct Params {
    side: f64,
    walls: u32,
    line_width: f64,
    initial_time: f64,
    decrease: f64,
    interval: u32,
}

impl Params {
    fn new(
        side: f64,
        walls: u32,
        line_width: f64,
        initial_time: f64,
        decrease: f64,
        interval: u32,
    ) -> Self {
        Self {
            side,
            walls,
            line_width,
            initial_time,
            decrease,
            interval,
        }
    }

    fn default() -> Self {
        Self::new(
            DEFAULT_SIDE,
            DEFAULT_WALLS,
            DEFAULT_LINE_WIDTH,
            DEFAULT_INITIAL_TIME,
            DEFAULT_DECREASE,
            DEFAULT_INTERVAL,
        )
    }
}

#[derive(Clone, Copy, PartialEq)]
enum ScreenKind {
    ModeSelect,
    DefaultConfirm,
    CustomInput,
    MarkerInput,
}

#[derive(Clone, Copy, PartialEq)]
enum Field {
    Side,
    Walls,
    LineWidth,
    InitialTime,
    Decrease,
    Interval,
}

enum Screen {
    ModeSelect,
    DefaultConfirm,
    CustomInput,
    MarkerInput,
    Result(ProcessResult),
    StartupError(String),
}

#[derive(Clone, Debug)]
struct ProcessResult {
    changed: usize,
    schedule: Vec<(i64, f64, f64)>,
    markers: Vec<(i64, f64)>,
}

struct ProcessOutcome {
    output: String,
    changed: usize,
    schedule: Vec<(i64, f64, f64)>,
    markers: Vec<(i64, f64)>,
}

// ===== 纯处理逻辑 =====

/// 判断一行是否为一个“纯数字”标记（仅由数字和小数点组成，例如 6 或 6.5）。
fn parse_marker(line: &str) -> Option<f64> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    if !trimmed.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    trimmed.parse::<f64>().ok()
}

fn is_layer_change(line: &str) -> bool {
    line.trim().starts_with(";LAYER_CHANGE")
}

/// 判断一行是否为“打印速度指令”：形如 `G1 F<数字>`（仅 G1 与 F 两个字段）。
fn is_speed_command(line: &str) -> bool {
    let code = line.split(';').next().unwrap_or("").trim();
    let tokens: Vec<&str> = code.split_whitespace().collect();
    if tokens.len() != 2 {
        return false;
    }
    if !tokens[0].eq_ignore_ascii_case("G1") {
        return false;
    }
    let p = tokens[1];
    if p.len() < 2 || !p[..1].eq_ignore_ascii_case("F") {
        return false;
    }
    p[1..].parse::<f64>().is_ok()
}

/// 将 `G1 F<数字>` 一行中的速度替换为给定速度，保留行首缩进、字段间空白与行尾注释。
fn set_speed_line(line: &str, speed: f64) -> String {
    let comment_idx = line.find(';');
    let (code, comment) = match comment_idx {
        Some(i) => (&line[..i], &line[i..]),
        None => (line, ""),
    };

    let tokens: Vec<&str> = code.split_whitespace().collect();
    if tokens.len() != 2 {
        return line.to_string();
    }
    let f_token = tokens[1];
    let f_token_start = code.find(f_token).unwrap_or(0);
    // F 参数的数字部分从第二个字符开始（跳过 'F'）。
    let number_start = f_token_start + 1;
    let number_end = f_token_start + f_token.len();

    let mut result = String::with_capacity(code.len() + 8);
    result.push_str(&code[..number_start]);
    result.push_str(&format_number(speed));
    result.push_str(&code[number_end..]);
    result.push_str(comment);
    result
}

fn format_number(v: f64) -> String {
    let rounded = (v * 1000.0).round() / 1000.0;
    let mut s = format!("{:.3}", rounded);
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    s
}

/// 根据层号计算自动模式下的目标层时间（秒）。
fn auto_layer_time(layer_idx: u32, params: &Params) -> f64 {
    let steps = (layer_idx / params.interval) as f64;
    let time = params.initial_time - steps * params.decrease;
    time.max(MIN_LAYER_TIME)
}

/// 根据方管边长、壁数、线宽与目标层时间计算打印速度（mm/min）。
fn compute_speed(side: f64, walls: u32, line_width: f64, layer_time: f64) -> f64 {
    let walls = walls as f64;
    // 每层墙壁路径总长度 = 4 * 墙数 * 边长 - 4 * 墙数 * (墙数 - 1) * 线宽
    // （内层壁每圈比外层壁短 2 个线宽，需要减去这部分路径缩短量）
    let perimeter = 4.0 * walls * side - 4.0 * walls * (walls - 1.0) * line_width;
    perimeter / layer_time * 60.0
}

/// 判断一行是否为 G1 移动指令。
fn is_g1(line: &str) -> bool {
    let code = line.split(';').next().unwrap_or("").trim();
    code.split_whitespace()
        .next()
        .map(|t| t.eq_ignore_ascii_case("G1"))
        .unwrap_or(false)
}

/// 提取一行 G-code 中某个参数（如 X/Y/E/F）的数值。
fn gcode_param(line: &str, param: u8) -> Option<f64> {
    let code = line.split(';').next().unwrap_or("").trim();
    for token in code.split_whitespace() {
        let bytes = token.as_bytes();
        if bytes.len() >= 2 && bytes[0].eq_ignore_ascii_case(&param) {
            if let Ok(v) = token[1..].parse::<f64>() {
                return Some(v);
            }
        }
    }
    None
}

/// 若该行是外壁挤出移动（G1 且带正的 E 值），返回其 X 坐标。
fn extrusion_x(line: &str) -> Option<f64> {
    if !is_g1(line) {
        return None;
    }
    let e = gcode_param(line, b'E')?;
    if e <= 0.0 {
        return None;
    }
    gcode_param(line, b'X')
}

/// 把一行 G1 移动指令中的 X 坐标增加 delta（mm），返回新行。
fn bump_x(line: &str, delta: f64) -> String {
    let comment_idx = line.find(';');
    let (code, comment) = match comment_idx {
        Some(i) => (&line[..i], &line[i..]),
        None => (line, ""),
    };

    let tokens: Vec<&str> = code.split_whitespace().collect();
    let Some(x_token) = tokens
        .iter()
        .copied()
        .find(|t| t.len() >= 2 && t.as_bytes()[0].eq_ignore_ascii_case(&b'X'))
    else {
        return line.to_string();
    };

    let start = code.find(x_token).unwrap_or(0);
    let number_start = start + 1;
    let number_end = start + x_token.len();

    let mut result = String::with_capacity(code.len() + 8);
    result.push_str(&code[..number_start]);
    match x_token[1..].parse::<f64>() {
        Ok(v) => result.push_str(&format_number(v + delta)),
        Err(_) => result.push_str(&x_token[1..]),
    }
    result.push_str(&code[number_end..]);
    result.push_str(comment);
    result
}

/// 在速度变化的层，把外壁（TYPE:Outer wall）中 X 坐标最大的连续两行挤出路径的
/// X 增大 MARKER_BUMP，作为该层速度变化的可见标记。
fn add_speed_change_markers(output: &str, speed_change_layers: &[i64]) -> String {
    let speed_change_set: std::collections::HashSet<i64> =
        speed_change_layers.iter().copied().collect();

    let mut result = String::with_capacity(output.len());
    let mut layer_idx: i64 = -1;
    let mut in_outer_wall = false;
    let mut outer_wall_buffer: Vec<String> = Vec::new();

    for line in output.split_inclusive('\n') {
        let (body, ending) = split_line_ending(line);
        let trimmed = body.trim();

        if trimmed.starts_with(";LAYER_CHANGE") {
            flush_outer_wall(
                &mut result,
                &mut outer_wall_buffer,
                layer_idx,
                &speed_change_set,
            );
            layer_idx += 1;
            in_outer_wall = false;
        }

        if trimmed.starts_with(";TYPE:Outer wall") {
            flush_outer_wall(
                &mut result,
                &mut outer_wall_buffer,
                layer_idx,
                &speed_change_set,
            );
            in_outer_wall = true;
        } else if in_outer_wall
            && (trimmed.starts_with(";TYPE:")
                || trimmed.starts_with(";WIPE_START")
                || trimmed.starts_with("EXCLUDE_OBJECT_END"))
        {
            flush_outer_wall(
                &mut result,
                &mut outer_wall_buffer,
                layer_idx,
                &speed_change_set,
            );
            in_outer_wall = false;
        }

        if in_outer_wall {
            outer_wall_buffer.push(format!("{}{}", body, ending));
        } else {
            result.push_str(body);
            result.push_str(ending);
        }
    }

    flush_outer_wall(
        &mut result,
        &mut outer_wall_buffer,
        layer_idx,
        &speed_change_set,
    );

    result
}

fn flush_outer_wall(
    result: &mut String,
    buffer: &mut Vec<String>,
    layer_idx: i64,
    speed_change_set: &std::collections::HashSet<i64>,
) {
    if buffer.is_empty() {
        return;
    }
    if speed_change_set.contains(&layer_idx) {
        bump_outer_wall_marker(buffer);
    }
    for line in buffer.drain(..) {
        result.push_str(&line);
    }
}

fn bump_outer_wall_marker(buffer: &mut [String]) {
    // 收集外壁挤出移动及其在 buffer 中的位置与 X 坐标
    let extrusions: Vec<(usize, f64)> = buffer
        .iter()
        .enumerate()
        .filter_map(|(i, line)| extrusion_x(line).map(|x| (i, x)))
        .collect();

    if extrusions.len() < 2 {
        return;
    }

    let max_x = extrusions
        .iter()
        .map(|(_, x)| *x)
        .fold(f64::NEG_INFINITY, f64::max);

    // 找 X == max_x 且在 buffer 中连续的两行，取最后一组
    let mut bump_indices: Option<Vec<usize>> = None;
    for w in extrusions.windows(2) {
        let (i0, x0) = w[0];
        let (i1, x1) = w[1];
        if i1 == i0 + 1 && (x0 - max_x).abs() < 1e-6 && (x1 - max_x).abs() < 1e-6 {
            bump_indices = Some(vec![i0, i1]);
        }
    }

    // 保底：没找到就用最后两行挤出
    let bump_indices = bump_indices.unwrap_or_else(|| {
        let n = extrusions.len();
        vec![extrusions[n - 2].0, extrusions[n - 1].0]
    });

    for idx in bump_indices {
        buffer[idx] = bump_x(&buffer[idx], MARKER_BUMP);
    }
}

fn split_line_ending(line: &str) -> (&str, &str) {
    if let Some(body) = line.strip_suffix("\r\n") {
        (body, "\r\n")
    } else if let Some(body) = line.strip_suffix('\n') {
        (body, "\n")
    } else {
        (line, "")
    }
}

fn process_gcode_contents(contents: &str, mode: Mode, params: &Params) -> ProcessOutcome {
    let mut output = String::with_capacity(contents.len());
    let mut current_override: Option<f64> = None; // 由纯数字标记指定的层时间
    let mut layer_idx: i64 = -1; // 当前层号（从 0 开始，与切片软件一致）
    let mut in_wipe = false; // 是否处于 WIPE 段（不改动擦嘴速度）
    let mut changed = 0usize;

    let mut schedule: Vec<(i64, f64, f64)> = Vec::new();
    let mut last_key: Option<(f64, f64)> = None;
    let mut markers: Vec<(i64, f64)> = Vec::new();

    for line in contents.split_inclusive('\n') {
        let (body, ending) = split_line_ending(line);
        let trimmed = body.trim();

        if is_layer_change(trimmed) {
            layer_idx += 1;
        }

        if trimmed.starts_with(";WIPE_START") {
            in_wipe = true;
        } else if trimmed.starts_with(";WIPE_END") {
            in_wipe = false;
        }

        // 纯数字标记：设置从该层起的层时间，并把该行转为注释（避免残留无效 G-code）。
        if let Some(v) = parse_marker(body) {
            current_override = Some(v);
            if layer_idx >= 0 {
                markers.push((layer_idx, v));
            }
            output.push_str(&format!("; layer_time_change: {}s", format_number(v)));
            output.push_str(ending);
            continue;
        }

        let mut new_body = body.to_string();

        if layer_idx >= 0 && !in_wipe && is_speed_command(body) {
            let layer_time = match current_override {
                Some(v) => Some(v.max(MIN_LAYER_TIME)),
                None => match mode {
                    Mode::Auto => Some(auto_layer_time(layer_idx as u32, params)),
                    Mode::Marker => None,
                },
            };
            if let Some(layer_time) = layer_time {
                let speed = compute_speed(params.side, params.walls, params.line_width, layer_time);
                new_body = set_speed_line(body, speed);
                if new_body != body {
                    changed += 1;
                    let key = (layer_time, speed);
                    if last_key != Some(key) {
                        schedule.push((layer_idx, layer_time, speed));
                        last_key = Some(key);
                    }
                }
            }
        }

        output.push_str(&new_body);
        output.push_str(ending);
    }

    // 在速度变化的层，给外壁加一个可见标记（X 偏移 MARKER_BUMP）
    let speed_change_layers: Vec<i64> = schedule.iter().map(|(layer, _, _)| *layer).collect();
    let output = add_speed_change_markers(&output, &speed_change_layers);

    ProcessOutcome {
        output,
        changed,
        schedule,
        markers,
    }
}

fn process_gcode_file(path: &Path, mode: Mode, params: &Params) -> io::Result<ProcessResult> {
    let contents = std::fs::read_to_string(path)?;
    let outcome = process_gcode_contents(&contents, mode, params);
    std::fs::write(path, &outcome.output)?;

    Ok(ProcessResult {
        changed: outcome.changed,
        schedule: outcome.schedule,
        markers: outcome.markers,
    })
}

// ===== 图形界面 =====

struct LayerTimeDialog {
    focus_handle: FocusHandle,
    gcode_path: Option<PathBuf>,
    file_name: SharedString,
    screen: Screen,
    selected_mode: ScreenKind,
    focused: Option<Field>,
    side_input: String,
    walls_input: String,
    line_width_input: String,
    initial_time_input: String,
    decrease_input: String,
    interval_input: String,
    error: Option<String>,
}

impl LayerTimeDialog {
    fn new(gcode_path: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let (file_name, startup_error) = match gcode_path.as_ref() {
            Some(path) => {
                let name = display_file_name(path);
                if path.is_file() {
                    (name.into(), None)
                } else {
                    (name.into(), Some(format!("文件不存在：\n{}", path.display())))
                }
            }
            None => (
                "未指定文件".into(),
                Some(
                    "未收到 G-code 文件路径。\n请在 OrcaSlicer 的「后处理脚本」中调用本程序，\n或手动传入 G-code 文件路径。"
                        .to_string(),
                ),
            ),
        };

        let screen = match startup_error {
            Some(message) => Screen::StartupError(message),
            None => Screen::ModeSelect,
        };

        Self {
            focus_handle: cx.focus_handle(),
            gcode_path,
            file_name,
            screen,
            selected_mode: ScreenKind::DefaultConfirm,
            focused: None,
            side_input: String::new(),
            walls_input: String::new(),
            line_width_input: String::new(),
            initial_time_input: String::new(),
            decrease_input: String::new(),
            interval_input: String::new(),
            error: None,
        }
    }

    fn go_to(&mut self, kind: ScreenKind, cx: &mut Context<Self>) {
        self.screen = match kind {
            ScreenKind::ModeSelect => Screen::ModeSelect,
            ScreenKind::DefaultConfirm => Screen::DefaultConfirm,
            ScreenKind::CustomInput => {
                self.side_input = format_number(DEFAULT_SIDE);
                self.walls_input = DEFAULT_WALLS.to_string();
                self.line_width_input = format_number(DEFAULT_LINE_WIDTH);
                self.initial_time_input = format_number(DEFAULT_INITIAL_TIME);
                self.decrease_input = format_number(DEFAULT_DECREASE);
                self.interval_input = DEFAULT_INTERVAL.to_string();
                Screen::CustomInput
            }
            ScreenKind::MarkerInput => {
                self.side_input = format_number(DEFAULT_SIDE);
                self.walls_input = DEFAULT_WALLS.to_string();
                self.line_width_input = format_number(DEFAULT_LINE_WIDTH);
                self.initial_time_input.clear();
                self.decrease_input.clear();
                self.interval_input.clear();
                Screen::MarkerInput
            }
        };
        self.focused = None;
        self.error = None;
        cx.notify();
    }

    fn focus_field(&mut self, field: Field, window: &mut Window, cx: &mut Context<Self>) {
        self.focused = Some(field);
        self.focus_handle.focus(window);
        cx.notify();
    }

    fn handle_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let modifiers = &event.keystroke.modifiers;

        match &self.screen {
            Screen::Result(_) | Screen::StartupError(_) => {
                if matches!(key, "enter" | "escape" | "space") {
                    cx.quit();
                }
                return;
            }
            _ => {}
        }

        if key == "escape" {
            if matches!(&self.screen, Screen::ModeSelect) {
                cx.quit();
            } else {
                self.go_to(ScreenKind::ModeSelect, cx);
            }
            return;
        }

        if key == "enter" {
            if matches!(
                &self.screen,
                Screen::DefaultConfirm | Screen::CustomInput | Screen::MarkerInput
            ) {
                self.submit(window, cx);
            }
            return;
        }

        let is_input = matches!(&self.screen, Screen::CustomInput | Screen::MarkerInput);
        if !is_input {
            return;
        }

        if key == "backspace" {
            if self.pop_focused() {
                self.error = None;
                cx.notify();
            }
            return;
        }

        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
            if modifiers.secondary() && key == "v" {
                self.paste(cx);
            }
            return;
        }

        let Some(text) = event.keystroke.key_char.as_deref() else {
            return;
        };

        let mut changed = false;
        for ch in text.chars() {
            if self.push_to_focused(ch) {
                changed = true;
            }
        }
        if changed {
            self.error = None;
            cx.notify();
        }
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let field = match self.focused {
            Some(f) => f,
            None => return,
        };
        let target = match field {
            Field::Side => &mut self.side_input,
            Field::Walls => &mut self.walls_input,
            Field::LineWidth => &mut self.line_width_input,
            Field::InitialTime => &mut self.initial_time_input,
            Field::Decrease => &mut self.decrease_input,
            Field::Interval => &mut self.interval_input,
        };

        let mut filtered = String::new();
        for ch in text.chars() {
            let valid = match field {
                Field::Walls | Field::Interval => ch.is_ascii_digit(),
                _ => (ch.is_ascii_digit() || ch == '.') && (ch != '.' || !filtered.contains('.')),
            };
            if valid && filtered.len() < 12 {
                filtered.push(ch);
            }
        }

        if !filtered.is_empty() {
            *target = filtered;
            self.error = None;
            cx.notify();
        }
    }

    fn pop_focused(&mut self) -> bool {
        match self.focused {
            Some(Field::Side) => self.side_input.pop().is_some(),
            Some(Field::Walls) => self.walls_input.pop().is_some(),
            Some(Field::LineWidth) => self.line_width_input.pop().is_some(),
            Some(Field::InitialTime) => self.initial_time_input.pop().is_some(),
            Some(Field::Decrease) => self.decrease_input.pop().is_some(),
            Some(Field::Interval) => self.interval_input.pop().is_some(),
            None => false,
        }
    }

    fn push_to_focused(&mut self, ch: char) -> bool {
        let field = match self.focused {
            Some(f) => f,
            None => return false,
        };
        let target = match field {
            Field::Side => &mut self.side_input,
            Field::Walls => &mut self.walls_input,
            Field::LineWidth => &mut self.line_width_input,
            Field::InitialTime => &mut self.initial_time_input,
            Field::Decrease => &mut self.decrease_input,
            Field::Interval => &mut self.interval_input,
        };
        let valid = match field {
            Field::Walls | Field::Interval => ch.is_ascii_digit(),
            _ => (ch.is_ascii_digit() || ch == '.') && (ch != '.' || !target.contains('.')),
        };
        if valid && target.len() < 12 {
            target.push(ch);
            true
        } else {
            false
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window);

        let Some(path) = self.gcode_path.clone() else {
            self.error = Some("没有可处理的 G-code 文件。".to_string());
            cx.notify();
            return;
        };

        let (mode, params) = match self.build_params() {
            Ok(value) => value,
            Err(message) => {
                self.error = Some(message);
                cx.notify();
                return;
            }
        };

        match process_gcode_file(&path, mode, &params) {
            Ok(result) => {
                println!("处理完成：修改 {} 处打印速度指令", result.changed);
                self.error = None;
                self.screen = Screen::Result(result);
            }
            Err(error) => {
                self.error = Some(format!("处理文件失败：{error}"));
            }
        }

        cx.notify();
    }

    fn build_params(&self) -> Result<(Mode, Params), String> {
        match &self.screen {
            Screen::DefaultConfirm => Ok((Mode::Auto, Params::default())),
            Screen::CustomInput => {
                let side = parse_f64(&self.side_input, "方管边长")?;
                if side <= 0.0 {
                    return Err("方管边长必须大于 0。".to_string());
                }
                let walls = parse_u32(&self.walls_input, "方管壁数")?;
                let line_width = parse_f64(&self.line_width_input, "线宽")?;
                if line_width <= 0.0 {
                    return Err("线宽必须大于 0。".to_string());
                }
                let initial_time = parse_f64(&self.initial_time_input, "初始层时间")?;
                if initial_time <= 0.0 {
                    return Err("初始层时间必须大于 0。".to_string());
                }
                let decrease = parse_f64(&self.decrease_input, "层时间变化量")?;
                if decrease < 0.0 {
                    return Err("层时间变化量不能为负。".to_string());
                }
                let interval = parse_u32(&self.interval_input, "间隔层数")?;
                Ok((
                    Mode::Auto,
                    Params::new(side, walls, line_width, initial_time, decrease, interval),
                ))
            }
            Screen::MarkerInput => {
                let side = parse_f64(&self.side_input, "方管边长")?;
                if side <= 0.0 {
                    return Err("方管边长必须大于 0。".to_string());
                }
                let walls = parse_u32(&self.walls_input, "方管壁数")?;
                let line_width = parse_f64(&self.line_width_input, "线宽")?;
                if line_width <= 0.0 {
                    return Err("线宽必须大于 0。".to_string());
                }
                Ok((
                    Mode::Marker,
                    Params::new(
                        side,
                        walls,
                        line_width,
                        DEFAULT_INITIAL_TIME,
                        DEFAULT_DECREASE,
                        DEFAULT_INTERVAL,
                    ),
                ))
            }
            _ => Err("当前界面无法提交。".to_string()),
        }
    }

    // ===== 渲染 =====

    fn card_title(&self, title: &'static str) -> AnyElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(div().w(px(3.0)).h(px(16.0)).rounded_full().bg(rgb(ACCENT)))
            .child(div().text_lg().text_color(rgb(TEXT)).child(title))
            .into_any_element()
    }

    fn file_box(&self) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(rgb(SUBTLE_BG))
            .border_1()
            .border_color(rgb(BOX_BORDER))
            .child(div().text_xs().text_color(rgb(MUTED)).child("G-code 文件"))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(TEXT))
                    .truncate()
                    .child(self.file_name.clone()),
            )
            .into_any_element()
    }

    fn hint_box(&self, text: &'static str) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .gap_1()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(rgb(SUBTLE_BG))
            .border_1()
            .border_color(rgb(BOX_BORDER))
            .child(div().text_xs().text_color(rgb(ACCENT)).child("提示"))
            .child(div().text_sm().text_color(rgb(MUTED)).child(text))
            .into_any_element()
    }

    fn section(&self, title: &'static str, children: Vec<AnyElement>) -> AnyElement {
        let mut inner = div().flex().flex_col().gap_2();
        for child in children {
            inner = inner.child(child);
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().text_sm().text_color(rgb(TEXT)).child(title))
            .child(
                inner
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(BOX_BORDER))
                    .bg(rgb(BOX_BG)),
            )
            .into_any_element()
    }

    fn row_elements(&self, rows: Vec<(String, String)>) -> Vec<AnyElement> {
        rows.into_iter()
            .map(|(label, value)| {
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .text_sm()
                    .child(div().text_color(rgb(MUTED)).child(label))
                    .child(div().text_color(rgb(TEXT)).child(value))
                    .into_any_element()
            })
            .collect()
    }

    fn actions(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            .justify_end()
            .items_center()
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(MUTED))
                    .mr_auto()
                    .child("回车应用 · Esc 返回"),
            )
            .child(
                div()
                    .id("back")
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(30.0))
                    .px_6()
                    .rounded_md()
                    .bg(rgb(SUBTLE_BG))
                    .border_1()
                    .border_color(rgb(BOX_BORDER))
                    .text_sm()
                    .text_color(rgb(TEXT))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(0xe9e9e9)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                        this.go_to(ScreenKind::ModeSelect, cx)
                    }))
                    .child("返回"),
            )
            .child(div().w(px(24.0)).h(px(1.0)))
            .child(
                div()
                    .id("apply")
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(30.0))
                    .px_6()
                    .rounded_md()
                    .bg(rgb(ACCENT))
                    .text_sm()
                    .text_color(rgb(ACCENT_TEXT))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(ACCENT_HOVER)))
                    .on_click(
                        cx.listener(|this, _: &ClickEvent, window, cx| this.submit(window, cx)),
                    )
                    .child("应用"),
            )
            .into_any_element()
    }

    fn mode_radio(
        &self,
        id: &'static str,
        title: &'static str,
        desc: &'static str,
        kind: ScreenKind,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.selected_mode == kind;
        div()
            .id(id)
            .flex()
            .items_center()
            .gap_3()
            .px_2()
            .py_2()
            .rounded_md()
            .cursor_pointer()
            .hover(|s| s.bg(rgb(SUBTLE_BG)))
            .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.selected_mode = kind;
                cx.notify();
            }))
            .child(
                div()
                    .size(px(16.0))
                    .rounded_full()
                    .border_1()
                    .border_color(if selected {
                        rgb(ACCENT)
                    } else {
                        rgb(BOX_BORDER)
                    })
                    .flex()
                    .items_center()
                    .justify_center()
                    .when(selected, |this| {
                        this.child(div().size(px(8.0)).rounded_full().bg(rgb(ACCENT)))
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_sm().text_color(rgb(TEXT)).child(title))
                    .child(div().text_xs().text_color(rgb(MUTED)).child(desc)),
            )
            .into_any_element()
    }

    fn input_field(
        &self,
        label: &'static str,
        placeholder: &'static str,
        value: &str,
        field: Field,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focused = self.focused == Some(field);
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(div().text_xs().text_color(rgb(MUTED)).child(label))
            .child(
                div()
                    .id(field_id(field))
                    .flex()
                    .items_center()
                    .gap_2()
                    .h(px(36.0))
                    .px_3()
                    .rounded_md()
                    .bg(rgb(BOX_BG))
                    .border_1()
                    .border_color(if focused {
                        rgb(ACCENT)
                    } else {
                        rgb(BOX_BORDER)
                    })
                    .cursor_text()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| this.focus_field(field, window, cx)),
                    )
                    .when(value.is_empty(), |this| {
                        this.child(
                            div()
                                .text_sm()
                                .text_color(rgb(BOX_BORDER))
                                .child(placeholder),
                        )
                    })
                    .when(!value.is_empty(), |this| {
                        this.child(
                            div()
                                .text_sm()
                                .text_color(rgb(TEXT))
                                .child(value.to_string()),
                        )
                    })
                    .when(focused, |this| this.child(cursor())),
            )
            .into_any_element()
    }

    fn render_mode_select(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("mode-select")
            .flex()
            .flex_col()
            .gap_4()
            .w(px(452.0))
            .p_6()
            .rounded_lg()
            .bg(rgb(PANEL_BG))
            .border_1()
            .border_color(rgb(BOX_BORDER))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_lg()
                            .text_color(rgb(TEXT))
                            .child("🥔🥔🥔欢迎使用蔬菜层时间自定义插件🥔🥔🥔"),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child("©20260910 shucai"),
                    ),
            )
            .child(self.file_box())
            .child(self.section(
                "模式",
                vec![
                    self.mode_radio(
                        "mode-default",
                        "默认参数",
                        "按内置参数自动生成层时间阶梯",
                        ScreenKind::DefaultConfirm,
                        cx,
                    ),
                    self.mode_radio(
                        "mode-custom",
                        "自定义参数",
                        "手动输入参数后自动生成层时间阶梯",
                        ScreenKind::CustomInput,
                        cx,
                    ),
                    self.mode_radio(
                        "mode-marker",
                        "使用切片预览中不同层高处输入的自定义层时间数值",
                        "读取 G-code 中该数值作为各层时间",
                        ScreenKind::MarkerInput,
                        cx,
                    ),
                ],
            ))
            .child(self.hint_box(PARAM_ALIGN_HINT))
            .child(
                div().flex().justify_end().child(
                    div()
                        .id("next")
                        .flex()
                        .items_center()
                        .justify_center()
                        .h(px(30.0))
                        .px_6()
                        .rounded_md()
                        .bg(rgb(ACCENT))
                        .text_sm()
                        .text_color(rgb(ACCENT_TEXT))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(ACCENT_HOVER)))
                        .on_click(cx.listener(|this, _: &ClickEvent, _window, cx| {
                            let target = this.selected_mode;
                            this.go_to(target, cx)
                        }))
                        .child("下一步"),
                ),
            )
            .into_any_element()
    }

    fn render_default_confirm(&self, cx: &mut Context<Self>) -> AnyElement {
        let geometry = vec![
            stat_row("方管边长", format!("{} mm", format_number(DEFAULT_SIDE))).into_any_element(),
            stat_row("方管壁数", format!("{} 层壁", DEFAULT_WALLS)).into_any_element(),
            stat_row("线宽", format!("{} mm", format_number(DEFAULT_LINE_WIDTH)))
                .into_any_element(),
        ];
        let ladder = vec![
            stat_row(
                "初始层时间",
                format!("{} s", format_number(DEFAULT_INITIAL_TIME)),
            )
            .into_any_element(),
            stat_row(
                "层时间变化量",
                format!("{} s", format_number(DEFAULT_DECREASE)),
            )
            .into_any_element(),
            stat_row("间隔层数", format!("{} 层", DEFAULT_INTERVAL)).into_any_element(),
        ];
        div()
            .id("default-confirm")
            .flex()
            .flex_col()
            .gap_4()
            .w(px(452.0))
            .p_6()
            .rounded_lg()
            .bg(rgb(PANEL_BG))
            .border_1()
            .border_color(rgb(BOX_BORDER))
            .child(self.card_title("默认参数"))
            .child(self.file_box())
            .child(self.section("方管参数", geometry))
            .child(self.section("层时间阶梯", ladder))
            .child(self.hint_box(PARAM_ALIGN_HINT))
            .when_some(self.error.as_ref(), |this, e| {
                this.child(div().text_sm().text_color(rgb(DANGER)).child(e.clone()))
            })
            .child(self.actions(cx))
            .into_any_element()
    }

    fn render_custom_input(&self, cx: &mut Context<Self>) -> AnyElement {
        let geometry = vec![
            self.input_field("方管边长 (mm)", "40", &self.side_input, Field::Side, cx),
            self.input_field("方管壁数（圈数）", "2", &self.walls_input, Field::Walls, cx),
            self.input_field(
                "线宽 (mm)",
                "0.42",
                &self.line_width_input,
                Field::LineWidth,
                cx,
            ),
        ];
        let ladder = vec![
            self.input_field(
                "初始层时间 (s)",
                "8",
                &self.initial_time_input,
                Field::InitialTime,
                cx,
            ),
            self.input_field(
                "层时间变化量 (s)",
                "0.5",
                &self.decrease_input,
                Field::Decrease,
                cx,
            ),
            self.input_field("间隔层数", "100", &self.interval_input, Field::Interval, cx),
        ];
        div()
            .id("custom-input")
            .flex()
            .flex_col()
            .gap_4()
            .w(px(452.0))
            .p_6()
            .rounded_lg()
            .bg(rgb(PANEL_BG))
            .border_1()
            .border_color(rgb(BOX_BORDER))
            .child(self.card_title("自定义参数"))
            .child(self.section("方管参数", geometry))
            .child(self.section("层时间阶梯", ladder))
            .child(self.hint_box(PARAM_ALIGN_HINT))
            .when_some(self.error.as_ref(), |this, e| {
                this.child(div().text_sm().text_color(rgb(DANGER)).child(e.clone()))
            })
            .child(self.actions(cx))
            .into_any_element()
    }

    fn render_marker_input(&self, cx: &mut Context<Self>) -> AnyElement {
        let geometry = vec![
            self.input_field("方管边长 (mm)", "40", &self.side_input, Field::Side, cx),
            self.input_field("方管壁数（圈数）", "2", &self.walls_input, Field::Walls, cx),
            self.input_field(
                "线宽 (mm)",
                "0.42",
                &self.line_width_input,
                Field::LineWidth,
                cx,
            ),
        ];
        div()
            .id("marker-input")
            .flex()
            .flex_col()
            .gap_4()
            .w(px(452.0))
            .p_6()
            .rounded_lg()
            .bg(rgb(PANEL_BG))
            .border_1()
            .border_color(rgb(BOX_BORDER))
            .child(self.card_title("切片中输入的参数"))
            .child(self.file_box())
            .child(div().text_sm().text_color(rgb(MUTED)).child(
                "层时间由 G-code 中插入的纯数字（如 6 或 6.5）决定，这里只需填写方管几何参数。",
            ))
            .child(self.section("方管参数", geometry))
            .when_some(self.error.as_ref(), |this, e| {
                this.child(div().text_sm().text_color(rgb(DANGER)).child(e.clone()))
            })
            .child(self.actions(cx))
            .into_any_element()
    }

    fn render_result(&self, result: &ProcessResult, cx: &mut Context<Self>) -> AnyElement {
        let mut el = div()
            .id("result-card")
            .flex()
            .flex_col()
            .gap_4()
            .w(px(452.0))
            .p_6()
            .rounded_lg()
            .bg(rgb(PANEL_BG))
            .border_1()
            .border_color(rgb(BOX_BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().size(px(8.0)).rounded_full().bg(rgb(SUCCESS)))
                    .child(div().text_lg().text_color(rgb(TEXT)).child("处理完成")),
            )
            .child(self.file_box())
            .child(self.section(
                "结果",
                vec![stat_row("修改速度指令", format!("{} 处", result.changed)).into_any_element()],
            ));

        if !result.schedule.is_empty() {
            let mut rows: Vec<(String, String)> = Vec::new();
            for (layer, time, speed) in result.schedule.iter().take(12) {
                rows.push((
                    format!("第 {} 层", layer),
                    format!(
                        "{} s → {} mm/min",
                        format_number(*time),
                        format_number(*speed)
                    ),
                ));
            }
            if result.schedule.len() > 12 {
                rows.push((String::new(), format!("… 共 {} 段", result.schedule.len())));
            }
            el = el.child(self.section("速度阶梯", self.row_elements(rows)));
        }

        if !result.markers.is_empty() {
            let rows: Vec<(String, String)> = result
                .markers
                .iter()
                .map(|(layer, v)| {
                    (
                        format!("第 {} 层", layer),
                        format!("{} s", format_number(*v)),
                    )
                })
                .collect();
            el = el.child(self.section("层时间标记", self.row_elements(rows)));
        }

        if result.changed == 0 {
            el = el.child(
                div()
                    .text_sm()
                    .text_color(rgb(DANGER))
                    .child("警告：未找到任何可修改的打印速度指令（G1 F...）。"),
            );
        }

        el.child(
            div()
                .flex()
                .justify_end()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .text_xs()
                        .text_color(rgb(MUTED))
                        .mr_auto()
                        .child("回车或 Esc 关闭"),
                )
                .child(
                    div()
                        .id("close")
                        .flex()
                        .items_center()
                        .justify_center()
                        .h(px(30.0))
                        .px_6()
                        .rounded_md()
                        .bg(rgb(ACCENT))
                        .text_sm()
                        .text_color(rgb(ACCENT_TEXT))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(ACCENT_HOVER)))
                        .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.quit()))
                        .child("关闭"),
                ),
        )
        .into_any_element()
    }

    fn render_startup_error(&self, message: &str, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id("error-card")
            .flex()
            .flex_col()
            .gap_4()
            .w(px(452.0))
            .p_6()
            .rounded_lg()
            .bg(rgb(PANEL_BG))
            .border_1()
            .border_color(rgb(BOX_BORDER))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().size(px(8.0)).rounded_full().bg(rgb(DANGER)))
                    .child(div().text_lg().text_color(rgb(TEXT)).child("无法开始处理")),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(MUTED))
                    .child(message.to_string()),
            )
            .child(
                div().flex().justify_end().child(
                    div()
                        .id("close-error")
                        .flex()
                        .items_center()
                        .justify_center()
                        .h(px(30.0))
                        .px_6()
                        .rounded_md()
                        .bg(rgb(SUBTLE_BG))
                        .border_1()
                        .border_color(rgb(BOX_BORDER))
                        .text_sm()
                        .text_color(rgb(TEXT))
                        .cursor_pointer()
                        .hover(|s| s.bg(rgb(0xe9e9e9)))
                        .on_click(cx.listener(|_, _: &ClickEvent, _, cx| cx.quit()))
                        .child("关闭"),
                ),
            )
            .into_any_element()
    }
}

impl Render for LayerTimeDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = match &self.screen {
            Screen::ModeSelect => self.render_mode_select(cx),
            Screen::DefaultConfirm => self.render_default_confirm(cx),
            Screen::CustomInput => self.render_custom_input(cx),
            Screen::MarkerInput => self.render_marker_input(cx),
            Screen::Result(result) => self.render_result(result, cx),
            Screen::StartupError(message) => self.render_startup_error(message, cx),
        };

        div()
            .size_full()
            .flex()
            .justify_center()
            .items_center()
            .bg(rgb(WINDOW_BG))
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::handle_key))
            .child(content)
    }
}

fn main() {
    let path = std::env::args_os().nth(1).map(PathBuf::from);

    Application::new().run(move |cx: &mut App| {
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(500.0), px(780.0)), cx);
        let path = path.clone();

        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("蔬菜层时间自定义插件".into()),
                    appears_transparent: false,
                    traffic_light_position: None,
                }),
                is_resizable: false,
                window_min_size: Some(size(px(500.0), px(780.0))),
                ..Default::default()
            },
            move |window, cx| {
                let view = cx.new(|cx| LayerTimeDialog::new(path.clone(), cx));
                view.read(cx).focus_handle.focus(window);
                view
            },
        )
        .expect("无法创建窗口");

        cx.activate(true);
    });
}

fn cursor() -> impl IntoElement {
    div().w(px(2.0)).h(px(18.0)).bg(rgb(ACCENT))
}

fn field_id(field: Field) -> &'static str {
    match field {
        Field::Side => "side-input",
        Field::Walls => "walls-input",
        Field::LineWidth => "line-width-input",
        Field::InitialTime => "initial-time-input",
        Field::Decrease => "decrease-input",
        Field::Interval => "interval-input",
    }
}

fn stat_row(label: &'static str, value: String) -> impl IntoElement {
    div()
        .flex()
        .justify_between()
        .items_center()
        .text_sm()
        .child(div().text_color(rgb(MUTED)).child(label))
        .child(div().text_color(rgb(TEXT)).child(value))
}

/// 切片软件会给后处理脚本传一个临时 `.pp` 副本，而 `SLIC3R_PP_OUTPUT_NAME`
/// 才是真正的输出 G-code 文件名。
fn display_file_name(gcode_path: &Path) -> String {
    let output_name = std::env::var_os("SLIC3R_PP_OUTPUT_NAME").map(PathBuf::from);
    output_name
        .filter(|p| p.file_name().is_some())
        .unwrap_or_else(|| gcode_path.to_path_buf())
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| gcode_path.display().to_string())
}

fn parse_f64(s: &str, label: &str) -> Result<f64, String> {
    s.trim()
        .parse::<f64>()
        .map_err(|_| format!("{label} 格式不正确。"))
}

fn parse_u32(s: &str, label: &str) -> Result<u32, String> {
    s.trim()
        .parse::<u32>()
        .map_err(|_| format!("{label} 格式不正确。"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    const SAMPLE: &str = "\
;LAYER_CHANGE
;TYPE:Outer wall
G1 F3000
G1 X10 Y10 E1
;LAYER_CHANGE
6
;TYPE:Outer wall
G1 F3000
G1 X10 Y10 E1
;WIPE_START
G1 F5000
;WIPE_END
";

    #[test]
    fn test_auto_layer_time() {
        let p = Params::default();
        assert!(close(auto_layer_time(0, &p), 8.0));
        assert!(close(auto_layer_time(99, &p), 8.0));
        assert!(close(auto_layer_time(100, &p), 7.5));
        assert!(close(auto_layer_time(200, &p), 7.0));
    }

    #[test]
    fn test_compute_speed() {
        // 单壁：无线宽修正，周长 = 4 * 40 = 160 mm
        assert!(close(compute_speed(40.0, 1, 0.42, 8.0), 1200.0));
        // 两层壁：周长 = 4*2*40 - 4*2*1*0.42 = 316.64 mm
        assert!(close(compute_speed(40.0, 2, 0.42, 8.0), 2374.8));
        assert!(close(compute_speed(40.0, 2, 0.42, 7.5), 2533.12));
        assert!(close(compute_speed(40.0, 2, 0.42, 6.0), 3166.4));
    }

    #[test]
    fn test_parse_marker() {
        assert_eq!(parse_marker("6"), Some(6.0));
        assert_eq!(parse_marker("6.5"), Some(6.5));
        assert_eq!(parse_marker("  6  "), Some(6.0));
        assert_eq!(parse_marker("G1 X10"), None);
        assert_eq!(parse_marker(";6"), None);
        assert_eq!(parse_marker(""), None);
    }

    #[test]
    fn test_is_speed_command() {
        assert!(is_speed_command("G1 F3000"));
        assert!(is_speed_command("G1 F3000 ; comment"));
        assert!(is_speed_command("  G1 F1200.5  "));
        assert!(!is_speed_command("G1 X10 Y10 F3000"));
        assert!(!is_speed_command("G1 E-.8 F1800"));
        assert!(!is_speed_command("G0 F3000"));
        assert!(!is_speed_command(";LAYER_CHANGE"));
    }

    #[test]
    fn test_set_speed_line() {
        assert_eq!(set_speed_line("G1 F3000", 1200.0), "G1 F1200");
        assert_eq!(set_speed_line("  G1 F3000 ; hi", 1280.0), "  G1 F1280 ; hi");
    }

    #[test]
    fn test_process_auto() {
        let params = Params::default();
        let outcome = process_gcode_contents(SAMPLE, Mode::Auto, &params);
        assert_eq!(outcome.changed, 2);
        assert!(outcome.output.contains("G1 F2374.8"));
        assert!(outcome.output.contains("G1 F3166.4"));
        assert!(outcome.output.contains("G1 F5000"));
        assert!(outcome.output.contains("; layer_time_change: 6s"));
        assert!(!outcome.output.lines().any(|l| l.trim() == "6"));
    }

    #[test]
    fn test_process_marker() {
        let params = Params::default();
        let outcome = process_gcode_contents(SAMPLE, Mode::Marker, &params);
        assert_eq!(outcome.changed, 1);
        assert!(outcome.output.contains("G1 F3000"));
        assert!(outcome.output.contains("G1 F3166.4"));
        assert!(outcome.output.contains("; layer_time_change: 6s"));
    }

    #[test]
    fn test_speed_change_marker() {
        // interval=1，使每一层都成为速度变化层
        let params = Params::new(40.0, 2, 0.42, 8.0, 0.5, 1);
        let sample = "\
;LAYER_CHANGE
;TYPE:Outer wall
G1 F3000
G1 X100 Y100 E1
G1 X100 Y120 E1
G1 X120 Y120 E1
G1 X120 Y100 E1
;LAYER_CHANGE
;TYPE:Outer wall
G1 F3000
G1 X100 Y100 E1
G1 X100 Y120 E1
G1 X120 Y120 E1
G1 X120 Y100 E1
";
        let outcome = process_gcode_contents(sample, Mode::Auto, &params);
        // 速度被修改
        assert!(outcome.output.contains("G1 F2374.8"));
        assert!(outcome.output.contains("G1 F2533.12"));
        // 外壁 X 最大的连续两行（X=120）被增大 0.1
        assert!(outcome.output.contains("X120.1 Y120"));
        assert!(outcome.output.contains("X120.1 Y100"));
        // 左边缘 X=100 保持不变
        assert!(outcome.output.contains("X100 Y100"));
        assert!(!outcome.output.contains("X120 Y120"));
    }
}
