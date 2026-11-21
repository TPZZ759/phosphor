-- tv_osd.lua — Pure-Black QD-OLED Netflix-Style OSD for Personal Windows 11 TV App
-- Loaded via: --osc=no --script=<app-dir>\mpv-scripts\tv_osd.lua
--
-- Supported --script-opts (prefix: tv_osd-):
--   tv_osd-title            (string)   Display title (falls back to media-title / filename)
--   tv_osd-sub              (string)   Subtitle / episode label (e.g. "S01E03 · Episode Name")
--   tv_osd-recap_s          (number)   Recap start time in seconds (-1 if none)
--   tv_osd-recap_e          (number)   Recap end time in seconds (-1 if none)
--   tv_osd-intro_s          (number)   Intro start time in seconds (-1 if none)
--   tv_osd-intro_e          (number)   Intro end time in seconds (-1 if none)
--   tv_osd-credits          (number)   Credits start time in seconds (-1 if none)
--   tv_osd-has_prev         (boolean)  Whether a previous episode is available
--   tv_osd-prev_label       (string)   Label for the Previous episode button/tooltip
--   tv_osd-has_next         (boolean)  Whether a next episode is available (yes/no/true/false/1/0)
--   tv_osd-next_label       (string)   Label for the Up Next card / Next button
--   tv_osd-autoplay         (boolean)  Enable Up Next auto-advance countdown
--   tv_osd-autoskip         (boolean)  Automatically skip recap/intro once on entering window
--   tv_osd-autoskip_credits (boolean)  Use 5s credits countdown (default yes) vs 10s
--   tv_osd-is_series        (boolean)  TV/anime series episode mode (smart Skip Start & credits fallback)
--   tv_osd-action_file      (string)   Path to mpv-action.json written synchronously before quit
--   tv_osd-episodes_file    (string)   Path to mpv-episodes.json containing current season episodes
--   tv_osd-shader_dir       (string)   Path to mpv-shaders directory (auto-resolved if empty)
--   tv_osd-shader_preset    (string)   Initial shader preset: off | film | anime | anime_hq
--   tv_osd-night            (boolean)  Enable Night Mode voice normalizer on startup
--   tv_osd-is_live          (boolean)  Live TV stream mode (hides VOD seek/intro/credits)
--   tv_osd-api_base         (string)   Local TV server base URL (default: http://127.0.0.1:7878)

local mp = require("mp")
local msg = require("mp.msg")
local options = require("mp.options")
local utils = require("mp.utils")
local assdraw = require("mp.assdraw")

--------------------------------------------------------------------------------
-- Constants & Palette (ASS uses &HBBGGRR&, alpha &H00& = opaque .. &HFF& = transparent)
--------------------------------------------------------------------------------
local FONT = "Segoe UI"
local NIGHT_FILTER_LABEL = "tv_night"
local NIGHT_FILTER_GRAPH = "dynaudnorm=f=150:g=15:p=0.90:m=8.0:s=12.0"
local NIGHT_FILTER_SPEC = "@" .. NIGHT_FILTER_LABEL .. ":lavfi=[" .. NIGHT_FILTER_GRAPH .. "]"
local CROPDETECT_LABEL = "tv_cropdetect"
local CROP_LABEL = "tv_crop"
local HIDE_DELAY = 2.5
local REPLAY_SEEK_SECS = 8
local REPLAY_RESTORE_SECS = 10

local C = {
    black        = "&H000000&", -- #000000 Pure OLED pixel-off black
    drawer_bg    = "&H070505&", -- #050507 Pure-black OLED drawer surface
    surface      = "&H0C0C0C&", -- #0C0C0C Deep card surface
    pill         = "&H161616&", -- #161616 Pill idle fill
    pill_hover   = "&H282828&", -- #282828 Pill hover fill
    border       = "&H303030&", -- #303030 Subtle crisp border
    border_hi    = "&H555555&", -- #555555 Hover border
    red          = "&H1409E5&", -- #E50914 Netflix Accent Red
    red_hover    = "&H1D12F6&", -- #F6121D Bright Hover Red
    red_dim      = "&H0D0748&", -- #48070D Active red pill tint
    white        = "&HFFFFFF&", -- #FFFFFF Pure white
    text         = "&HEAEAEA&", -- #EAEAEA Crisp primary text
    muted        = "&HA0A0A0&", -- #A0A0A0 Secondary text
    dim          = "&H666666&", -- #666666 Subtle track/hint
    track_bg     = "&H2C2C2C&", -- #2C2C2C Seek bar track
    track_cache  = "&H4A4A4A&", -- #4A4A4A Buffered range
    cyan         = "&HFFD400&", -- #00D4FF Intro marker highlight
    purple       = "&HF57CA8&", -- #A87CF5 Recap marker highlight
    amber        = "&H00B3FF&", -- #FFB300 Credits / Filler highlight
    amber_dim    = "&H082D42&", -- #422D08 Subtle amber pill tint
}

local SHADER_PRESET_ORDER = { "off", "film", "anime", "anime_hq" }
local SHADER_PRESETS = {
    off = {
        id = "off",
        pill = "Shader: Off",
        toast = "Shaders: Off",
        files = {},
    },
    film = {
        id = "film",
        pill = "Shader: Film",
        toast = "Shader: Film (FSRCNNX + KrigBilateral)",
        files = {
            "FSRCNNX_x2_8-0-4-1.glsl",
            "KrigBilateral.glsl",
        },
    },
    anime = {
        id = "anime",
        pill = "Shader: Anime4K",
        toast = "Shader: Anime4K (Mode A)",
        files = {
            "Anime4K_Clamp_Highlights.glsl",
            "Anime4K_Restore_CNN_M.glsl",
            "Anime4K_Upscale_CNN_x2_M.glsl",
            "Anime4K_AutoDownscalePre_x2.glsl",
            "Anime4K_AutoDownscalePre_x4.glsl",
            "Anime4K_Upscale_CNN_x2_S.glsl",
        },
    },
    anime_hq = {
        id = "anime_hq",
        pill = "Shader: A4K HQ",
        toast = "Shader: Anime4K HQ (Mode A VL)",
        files = {
            "Anime4K_Clamp_Highlights.glsl",
            "Anime4K_Restore_CNN_VL.glsl",
            "Anime4K_Upscale_CNN_x2_VL.glsl",
            "Anime4K_AutoDownscalePre_x2.glsl",
            "Anime4K_AutoDownscalePre_x4.glsl",
            "Anime4K_Upscale_CNN_x2_M.glsl",
        },
    },
}

--------------------------------------------------------------------------------
-- Options (parsed as strings first so yes/no, true/false, 1/0, empty all work cleanly)
--------------------------------------------------------------------------------
local raw_opts = {
    title            = "",
    sub              = "",
    recap_s          = "-1",
    recap_e          = "-1",
    intro_s          = "-1",
    intro_e          = "-1",
    credits          = "-1",
    has_prev         = "no",
    prev_label       = "",
    has_next         = "no",
    next_label       = "",
    autoplay         = "no",
    autoskip         = "no",
    autoskip_credits = "yes",
    is_series        = "no",
    action_file      = "",
    episodes_file    = "",
    shader_dir       = "",
    shader_preset    = "off",
    night            = "no",
    is_live          = "no",
    api_base         = "http://127.0.0.1:7878",
}

local opts = {
    title            = "",
    sub              = "",
    recap_s          = -1,
    recap_e          = -1,
    intro_s          = -1,
    intro_e          = -1,
    credits          = -1,
    has_prev         = false,
    prev_label       = "",
    has_next         = false,
    next_label       = "",
    autoplay         = false,
    autoskip         = false,
    autoskip_credits = true,
    is_series        = false,
    action_file      = "",
    episodes_file    = "",
    shader_dir       = "",
    shader_preset    = "off",
    night            = false,
    is_live          = false,
    api_base         = "http://127.0.0.1:7878",
}

local function parse_bool(val, default_val)
    if type(val) == "boolean" then return val end
    if val == nil then return default_val end
    local s = tostring(val):lower():gsub("^%s+", ""):gsub("%s+$", "")
    if s == "" then return default_val end
    if s == "yes" or s == "true" or s == "1" or s == "on" then return true end
    if s == "no" or s == "false" or s == "0" or s == "off" then return false end
    return default_val
end

local function parse_num(val, default_val)
    if type(val) == "number" then return val end
    if val == nil or val == "" then return default_val end
    local n = tonumber(val)
    if n == nil then return default_val end
    return n
end

local function normalize_preset(val)
    local s = tostring(val or "off"):lower():gsub("^%s+", ""):gsub("%s+$", "")
    if SHADER_PRESETS[s] then return s end
    return "off"
end

local function resolve_shader_dir()
    if opts.shader_dir and opts.shader_dir ~= "" then
        return opts.shader_dir:gsub("[/\\]+$", "")
    end
    local script_dir = mp.get_script_directory()
    if script_dir and script_dir ~= "" then
        local parent = script_dir:gsub("[/\\]+$", ""):gsub("[/\\][^/\\]+$", "")
        if parent and parent ~= "" then
            return parent .. "\\mpv-shaders"
        end
    end
    return "mpv-shaders"
end

local function sync_parsed_opts()
    opts.title            = tostring(raw_opts.title or "")
    opts.sub              = tostring(raw_opts.sub or "")
    opts.recap_s          = parse_num(raw_opts.recap_s, -1)
    opts.recap_e          = parse_num(raw_opts.recap_e, -1)
    opts.intro_s          = parse_num(raw_opts.intro_s, -1)
    opts.intro_e          = parse_num(raw_opts.intro_e, -1)
    opts.credits          = parse_num(raw_opts.credits, -1)
    opts.has_prev         = parse_bool(raw_opts.has_prev, false)
    opts.prev_label       = tostring(raw_opts.prev_label or "")
    opts.has_next         = parse_bool(raw_opts.has_next, false)
    opts.next_label       = tostring(raw_opts.next_label or "")
    opts.autoplay         = parse_bool(raw_opts.autoplay, false)
    opts.autoskip         = parse_bool(raw_opts.autoskip, false)
    opts.autoskip_credits = parse_bool(raw_opts.autoskip_credits, true)
    opts.is_series        = parse_bool(raw_opts.is_series, false)
    opts.action_file      = tostring(raw_opts.action_file or "")
    opts.episodes_file    = tostring(raw_opts.episodes_file or "")
    opts.shader_dir       = tostring(raw_opts.shader_dir or "")
    opts.shader_preset    = normalize_preset(raw_opts.shader_preset)
    opts.night            = parse_bool(raw_opts.night, false)
    opts.is_live          = parse_bool(raw_opts.is_live, false)
    opts.api_base         = tostring(raw_opts.api_base or "http://127.0.0.1:7878")
end

--------------------------------------------------------------------------------
-- State
--------------------------------------------------------------------------------
local overlay = mp.create_osd_overlay("ass-events")
local overlay_shown = false
local last_ass_data = nil

local state = {
    -- Canvas geometry
    osd_w = 1920,
    osd_h = 1080,
    res_x = 1920,
    res_y = 1080,

    -- HUD & Mouse
    hud_visible = false,
    mouse_x = -1,
    mouse_y = -1,
    mouse_hover = false,
    last_raw_mx = nil,
    last_raw_my = nil,
    dragging_seek = false,
    dragging_vol = false,
    seek_drag_time = nil,

    -- Playback
    paused = false,
    time_pos = 0,
    duration = 0,
    cache_time = 0,
    speed = 1.0,
    volume = 100,
    mute = false,
    media_title = "",
    tracks = {},
    chapters = {},
    vid_w = 0,
    vid_h = 0,
    sub_visibility = true,

    -- Features
    night_active = false,
    shader_preset = "off",
    crop_mode = "off", -- "off" | "detecting" | "active"
    crop_detected = nil,
    crop_samples = 0,

    -- Replay ("What did they just say?")
    replay_active = false,
    replay_prev_sid = "no",
    replay_prev_vis = true,

    -- Smart Skip (Recap / Intro / Skip Start)
    skip_pill_mode = nil,   -- nil | "recap" | "intro" | "start"
    skip_pill_label = nil,
    skip_pill_target = nil,
    recap_autoskipped = false,
    recap_dismissed = false,
    intro_autoskipped = false,
    intro_dismissed = false,
    skip_start_dismissed = false,

    -- Up Next & Credits Exit
    up_next_visible = false,
    up_next_dismissed = false,
    up_next_total = 5,
    up_next_remaining = 5,
    up_next_last_tick = nil,
    up_next_enter_bound = false,
    credits_exit_visible = false,
    credits_exit_dismissed = false,
    quitting_for_next = false,

    -- Episodes Drawer
    episodes = {},
    episodes_loaded_path = "",
    ep_drawer_open = false,
    ep_scroll = 0,
    ep_max_scroll = 0,
    ep_drawer_box = nil,
    drawer_esc_bound = false,
    drawer_wheel_bound = false,

    -- Toast
    toast_text = nil,
    toast_expire = 0,

    -- Interactive hit regions rebuilt on every layout
    hitboxes = {},
    seek_box = nil,
    vol_box = nil,
    top_bar_y1 = 98,
    bot_bar_y0 = 914,
}

local hide_timer = nil
local replay_timer = nil
local crop_timer = nil
local toast_timer = nil
local click_pause_timer = nil
local render -- forward declaration

--------------------------------------------------------------------------------
-- Formatting & Utility Helpers
--------------------------------------------------------------------------------
local function clamp(v, lo, hi)
    if v < lo then return lo end
    if v > hi then return hi end
    return v
end

local function ass_escape(str)
    if not str then return "" end
    str = tostring(str)
    str = str:gsub("\\", "\\\239\187\191")
    str = str:gsub("{", "\\{")
    str = str:gsub("}", "\\}")
    str = str:gsub("\r\n", " "):gsub("[\r\n]", " ")
    return str
end

local function truncate_text(str, max_chars)
    if not str then return "" end
    if #str <= max_chars then return str end
    return str:sub(1, math.max(1, max_chars - 3)) .. "..."
end

local function fmt_time(secs)
    if not secs or secs ~= secs or secs < 0 then secs = 0 end
    local total = math.floor(secs + 0.5)
    local h = math.floor(total / 3600)
    local m = math.floor((total % 3600) / 60)
    local s = total % 60
    if h > 0 then
        return string.format("%d:%02d:%02d", h, m, s)
    end
    return string.format("%02d:%02d", m, s)
end

local function fmt_clock(epoch_secs)
    local t = os.date("*t", epoch_secs)
    if not t then return "" end
    local hour12 = t.hour % 12
    if hour12 == 0 then hour12 = 12 end
    local ampm = (t.hour >= 12) and "PM" or "AM"
    return string.format("%d:%02d %s", hour12, t.min, ampm)
end

local function estimate_text_w(str, font_size)
    if not str or str == "" then return 0 end
    local units = 0
    local i = 1
    local len = #str
    while i <= len do
        local b = str:byte(i)
        if b >= 128 then
            -- Advance across multi-byte UTF-8 sequence as one glyph
            if b >= 240 then i = i + 4
            elseif b >= 224 then i = i + 3
            elseif b >= 192 then i = i + 2
            else i = i + 1 end
            units = units + 0.45
        else
            local ch = str:sub(i, i)
            if ch:match("[ilI1!.,:;'| ]") then
                units = units + 0.30
            elseif ch:match("[MWQG@#%%]") then
                units = units + 0.78
            elseif ch:match("[A-Z]") then
                units = units + 0.62
            elseif ch:match("[0-9]") then
                units = units + 0.55
            else
                units = units + 0.50
            end
            i = i + 1
        end
    end
    return math.ceil(units * font_size)
end

local function truncate_to_width(str, font_size, max_w)
    if not str or str == "" then return "" end
    if estimate_text_w(str, font_size) <= max_w then return str end
    local ell_w = estimate_text_w("...", font_size)
    local budget = math.max(12, max_w - ell_w)
    local out_end = 0
    local units = 0
    local i = 1
    local len = #str
    while i <= len do
        local b = str:byte(i)
        local step = 1
        local w = 0.50
        if b >= 128 then
            if b >= 240 then step = 4
            elseif b >= 224 then step = 3
            elseif b >= 192 then step = 2 end
            w = 0.45
        else
            local ch = str:sub(i, i)
            if ch:match("[ilI1!.,:;'| ]") then w = 0.30
            elseif ch:match("[MWQG@#%%]") then w = 0.78
            elseif ch:match("[A-Z]") then w = 0.62
            elseif ch:match("[0-9]") then w = 0.55 end
        end
        if (units + w) * font_size > budget then
            break
        end
        units = units + w
        out_end = math.min(len, i + step - 1)
        i = i + step
    end
    if out_end <= 0 then return "..." end
    return str:sub(1, out_end):gsub("%s+$", "") .. "..."
end

local function point_in_rect(px, py, r)
    return r and px >= r.x0 and px <= r.x1 and py >= r.y0 and py <= r.y1
end

local function file_exists(path)
    local f = io.open(path, "r")
    if f then
        f:close()
        return true
    end
    return false
end

local function show_toast(text, duration)
    state.toast_text = text
    state.toast_expire = mp.get_time() + (duration or 2.4)
    if toast_timer then
        toast_timer:kill()
        toast_timer = nil
    end
    toast_timer = mp.add_timeout((duration or 2.4) + 0.05, function()
        if mp.get_time() >= state.toast_expire then
            state.toast_text = nil
            render()
        end
    end)
    render()
end

--------------------------------------------------------------------------------
-- Canvas & Hitbox Helpers
--------------------------------------------------------------------------------
local function update_canvas_size()
    local ow, oh = mp.get_osd_size()
    if not ow or ow <= 0 or not oh or oh <= 0 then
        ow, oh = 1920, 1080
    end
    state.osd_w = ow
    state.osd_h = oh
    local aspect = ow / oh
    if aspect <= 0 then aspect = 16 / 9 end
    local ry = 1080
    local rx = math.floor(ry * aspect + 0.5)
    if rx < 1200 then
        rx = 1200
        ry = math.floor(rx / aspect + 0.5)
    end
    state.res_x = rx
    state.res_y = ry
end

local function to_canvas_coords(mx, my)
    if state.osd_w <= 0 or state.osd_h <= 0 then return -1, -1 end
    return mx * (state.res_x / state.osd_w), my * (state.res_y / state.osd_h)
end

--------------------------------------------------------------------------------
-- Dynamic Keybinding Helpers (ESC / Wheel for Drawer, Enter for Up Next)
--------------------------------------------------------------------------------
local close_ep_drawer -- forward declaration
local trigger_play_next -- forward declaration

local function set_up_next_enter_bound(active)
    if active and not state.up_next_enter_bound then
        mp.add_forced_key_binding("ENTER", "tv_osd_upnext_enter", function()
            if state.up_next_visible and opts.has_next then trigger_play_next() end
        end)
        mp.add_forced_key_binding("KP_ENTER", "tv_osd_upnext_kp_enter", function()
            if state.up_next_visible and opts.has_next then trigger_play_next() end
        end)
        state.up_next_enter_bound = true
    elseif not active and state.up_next_enter_bound then
        mp.remove_key_binding("tv_osd_upnext_enter")
        mp.remove_key_binding("tv_osd_upnext_kp_enter")
        state.up_next_enter_bound = false
    end
end

local function set_drawer_esc_bound(active)
    if active and not state.drawer_esc_bound then
        mp.add_forced_key_binding("ESC", "tv_osd_drawer_esc", function()
            close_ep_drawer()
        end)
        state.drawer_esc_bound = true
    elseif not active and state.drawer_esc_bound then
        mp.remove_key_binding("tv_osd_drawer_esc")
        state.drawer_esc_bound = false
    end
end

local function scroll_ep_drawer(delta)
    if not state.ep_drawer_open then return end
    local next_scroll = clamp((state.ep_scroll or 0) + delta, 0, state.ep_max_scroll or 0)
    if next_scroll ~= state.ep_scroll then
        state.ep_scroll = next_scroll
        render()
    end
end

local function set_drawer_wheel_bound(active)
    if active and not state.drawer_wheel_bound then
        mp.add_forced_key_binding("WHEEL_UP", "tv_osd_drawer_wheel_up", function()
            scroll_ep_drawer(-1)
        end)
        mp.add_forced_key_binding("WHEEL_DOWN", "tv_osd_drawer_wheel_down", function()
            scroll_ep_drawer(1)
        end)
        state.drawer_wheel_bound = true
    elseif not active and state.drawer_wheel_bound then
        mp.remove_key_binding("tv_osd_drawer_wheel_up")
        mp.remove_key_binding("tv_osd_drawer_wheel_down")
        state.drawer_wheel_bound = false
    end
end

local function update_drawer_wheel_binding()
    local over = state.ep_drawer_open
        and state.mouse_hover
        and state.ep_drawer_box ~= nil
        and point_in_rect(state.mouse_x, state.mouse_y, state.ep_drawer_box)
    set_drawer_wheel_bound(over)
end

--------------------------------------------------------------------------------
-- Race-Free Episode Action Writer (mpv-action.json)
--------------------------------------------------------------------------------
local function write_action_and_quit(payload_json, toast_msg, is_next)
    if state.quitting_for_next then return end
    state.quitting_for_next = true
    state.up_next_visible = false
    state.credits_exit_visible = false
    state.skip_pill_mode = nil
    if state.ep_drawer_open then
        state.ep_drawer_open = false
        set_drawer_esc_bound(false)
        set_drawer_wheel_bound(false)
    end
    set_up_next_enter_bound(false)

    if opts.action_file and opts.action_file ~= "" then
        local f, err = io.open(opts.action_file, "wb")
        if f then
            f:write(payload_json)
            f:flush()
            f:close()
        else
            msg.warn("Failed to write action_file (" .. opts.action_file .. "): " .. tostring(err))
        end
    end

    if is_next or (payload_json and payload_json:find('"action":"next"', 1, true)) then
        mp.set_property("user-data/tv-next", "1")
    end

    show_toast(toast_msg or "Switching Episode...", 1.0)
    mp.add_timeout(0.08, function()
        mp.command("quit")
    end)
end

trigger_play_next = function()
    if not opts.has_next then return end
    write_action_and_quit('{"action":"next"}', "Starting Next Episode...", true)
end

local function trigger_play_prev()
    if not opts.has_prev then return end
    write_action_and_quit('{"action":"prev"}', "Starting Previous Episode...", false)
end

local function trigger_play_episode(ep_id, ep_label)
    local num_id = tonumber(ep_id)
    local payload
    if num_id then
        if math.abs(num_id) < 2147483647 then
            payload = string.format('{"action":"ep","id":%d}', num_id)
        else
            payload = string.format('{"action":"ep","id":%.0f}', num_id)
        end
    else
        local safe_id = tostring(ep_id or ""):gsub("\\", "\\\\"):gsub('"', '\\"')
        payload = string.format('{"action":"ep","id":"%s"}', safe_id)
    end
    write_action_and_quit(payload, "Playing " .. (ep_label or "Episode") .. "...", false)
end

--------------------------------------------------------------------------------
-- Episodes List Loader & Drawer State
--------------------------------------------------------------------------------
local function load_episodes_file(force)
    local path = opts.episodes_file or ""
    if not force and path == state.episodes_loaded_path then
        return
    end
    state.episodes_loaded_path = path
    state.episodes = {}
    state.ep_scroll = 0
    state.ep_max_scroll = 0

    if path == "" then return end

    local f = io.open(path, "rb")
    if not f then return end
    local raw = f:read("*a")
    f:close()
    if not raw or raw == "" then return end

    raw = raw:gsub("^\239\187\191", "") -- Strip optional UTF-8 BOM
    local parsed, err = utils.parse_json(raw)
    if type(parsed) ~= "table" then
        if err then
            msg.warn("Failed to parse episodes_file (" .. path .. "): " .. tostring(err))
        end
        return
    end

    local list = {}
    for idx, item in ipairs(parsed) do
        if type(item) == "table" and item.id ~= nil then
            list[#list + 1] = {
                id       = item.id,
                season   = tonumber(item.season) or 1,
                n        = tonumber(item.n) or idx,
                title    = (type(item.title) == "string") and item.title or "",
                progress = tonumber(item.progress) or 0,
                current  = (item.current == true),
                filler   = (type(item.filler) == "string") and item.filler:lower() or "",
            }
        end
    end
    state.episodes = list
end

local schedule_hide -- forward declaration

local function open_ep_drawer()
    if #state.episodes == 0 then
        show_toast("No episode list available")
        return
    end
    state.ep_drawer_open = true
    state.hud_visible = true
    if hide_timer then
        hide_timer:kill()
        hide_timer = nil
    end

    -- Auto-scroll to show current episode comfortably near top
    local cur_idx = nil
    for i, ep in ipairs(state.episodes) do
        if ep.current then
            cur_idx = i
            break
        end
    end
    if cur_idx then
        state.ep_scroll = math.max(0, cur_idx - 3)
    end

    set_drawer_esc_bound(true)
    render()
    update_drawer_wheel_binding()
end

close_ep_drawer = function()
    if not state.ep_drawer_open then return end
    state.ep_drawer_open = false
    state.ep_drawer_box = nil
    set_drawer_esc_bound(false)
    set_drawer_wheel_bound(false)
    schedule_hide()
    render()
end

local function toggle_ep_drawer()
    if state.ep_drawer_open then
        close_ep_drawer()
    else
        open_ep_drawer()
    end
end

--------------------------------------------------------------------------------
-- Native MKV/MP4 Chapter Detection
--------------------------------------------------------------------------------
local function detect_chapters()
    local raw_chapters = mp.get_property_native("chapter-list")
    if type(raw_chapters) ~= "table" or #raw_chapters == 0 then
        state.chapters = {}
        return
    end

    local list = {}
    for _, ch in ipairs(raw_chapters) do
        if type(ch) == "table" and tonumber(ch.time) then
            list[#list + 1] = {
                time  = tonumber(ch.time),
                title = (type(ch.title) == "string") and ch.title or "",
            }
        end
    end
    table.sort(list, function(a, b) return a.time < b.time end)
    state.chapters = list

    if opts.is_live then return end

    local dur = (state.duration and state.duration > 0)
        and state.duration
        or (mp.get_property_number("duration", 0) or 0)

    local matched_recap_idx = nil
    for i, ch in ipairs(list) do
        local ch_start = ch.time
        local ch_end = (list[i + 1] and list[i + 1].time) or (dur > 0 and dur or nil)
        local ch_dur = (ch_end and ch_end > ch_start) and (ch_end - ch_start) or nil
        local ch_title = ch.title:lower()
        local padded = " " .. ch_title:gsub("[^%w]+", " ") .. " "

        -- 1. Recap chapter: recap|previously (duration < 240s, starting < 240s)
        if opts.recap_s < 0 and ch_start >= 0 and ch_start < 240 and ch_dur and ch_dur > 0 and ch_dur < 240 then
            if ch_title:find("recap", 1, true) or ch_title:find("previously", 1, true) then
                opts.recap_s = ch_start
                opts.recap_e = ch_end
                matched_recap_idx = i
            end
        end

        -- 2. Intro chapter: intro|opening|title sequence|prologue|\bop\b (duration < 240s, starting < 420s)
        if opts.intro_s < 0 and matched_recap_idx ~= i
            and ch_start >= 0 and ch_start < 420 and ch_dur and ch_dur > 0 and ch_dur < 240 then
            if ch_title:find("intro", 1, true)
                or ch_title:find("opening", 1, true)
                or ch_title:find("title sequence", 1, true)
                or ch_title:find("prologue", 1, true)
                or padded:find(" op ", 1, true) then
                opts.intro_s = ch_start
                opts.intro_e = ch_end
            end
        end

        -- 3. Credits chapter: credits|outro|ending|end credits|closing|preview|next episode|\bed\b (after 55% of duration)
        if opts.credits < 0 and dur > 0 and ch_start >= (dur * 0.55) and ch_start < dur then
            if ch_title:find("credits", 1, true)
                or ch_title:find("outro", 1, true)
                or ch_title:find("ending", 1, true)
                or ch_title:find("end credits", 1, true)
                or ch_title:find("closing", 1, true)
                or ch_title:find("preview", 1, true)
                or ch_title:find("next episode", 1, true)
                or padded:find(" ed ", 1, true) then
                opts.credits = ch_start
            end
        end
    end
end

local function find_chapter_at(t)
    if not state.chapters or #state.chapters == 0 then return nil end
    local found = nil
    for _, ch in ipairs(state.chapters) do
        if ch.time <= t then
            found = ch
        else
            break
        end
    end
    return found
end

--------------------------------------------------------------------------------
-- Audio / Subtitle Track Helpers
--------------------------------------------------------------------------------
local function get_audio_tracks()
    local list = {}
    for _, t in ipairs(state.tracks) do
        if t.type == "audio" then
            list[#list + 1] = t
        end
    end
    return list
end

local function get_sub_tracks()
    local list = {}
    for _, t in ipairs(state.tracks) do
        if t.type == "sub" then
            list[#list + 1] = t
        end
    end
    return list
end

local function fmt_channels(ch)
    if not ch then return "" end
    if ch == 6 then return "5.1" end
    if ch == 8 then return "7.1" end
    if ch == 2 then return "2.0" end
    if ch == 1 then return "1.0" end
    return tostring(ch) .. "ch"
end

local function get_audio_pill_label()
    local list = get_audio_tracks()
    if #list == 0 then return "Audio: None" end
    for _, t in ipairs(list) do
        if t.selected then
            local lang = (t.lang and t.lang ~= "") and t.lang:upper():sub(1, 3) or ("#" .. tostring(t.id))
            local ch = fmt_channels(t["audio-channels"])
            if ch ~= "" then
                return "Audio: " .. lang .. " " .. ch
            end
            return "Audio: " .. lang
        end
    end
    return "Audio: Off"
end

local function get_sub_pill_label()
    local list = get_sub_tracks()
    if #list == 0 then return "Sub: None" end
    if not state.sub_visibility then return "Sub: Off" end
    for _, t in ipairs(list) do
        if t.selected then
            local lang = (t.lang and t.lang ~= "") and t.lang:upper():sub(1, 3)
                or (t.title and t.title ~= "" and truncate_text(t.title, 6))
                or ("#" .. tostring(t.id))
            return "Sub: " .. lang
        end
    end
    return "Sub: Off"
end

local function cycle_audio_track()
    local list = get_audio_tracks()
    if #list == 0 then
        show_toast("No audio tracks")
        return
    end
    if #list == 1 then
        local t = list[1]
        local lang = (t.lang and t.lang ~= "") and t.lang:upper() or ("Track " .. tostring(t.id))
        show_toast("Audio: " .. lang .. " (only track)")
        return
    end
    local cur_idx = 0
    for i, t in ipairs(list) do
        if t.selected then
            cur_idx = i
            break
        end
    end
    local next_t = list[(cur_idx % #list) + 1]
    mp.set_property_number("aid", next_t.id)
    local lang = (next_t.lang and next_t.lang ~= "") and next_t.lang:upper() or ("Track " .. tostring(next_t.id))
    local ch = fmt_channels(next_t["audio-channels"])
    local extra = (ch ~= "") and (" · " .. ch) or ""
    if next_t.title and next_t.title ~= "" then
        extra = extra .. " · " .. truncate_text(next_t.title, 24)
    end
    show_toast("Audio: " .. lang .. extra)
end

local function cancel_replay_timer(restore_now)
    if replay_timer then
        replay_timer:kill()
        replay_timer = nil
    end
    if restore_now and state.replay_active then
        if state.replay_prev_sid == nil or state.replay_prev_sid == "no" or state.replay_prev_sid == false then
            mp.set_property("sid", "no")
        else
            mp.set_property("sid", tostring(state.replay_prev_sid))
        end
        mp.set_property_bool("sub-visibility", state.replay_prev_vis ~= false)
    end
    state.replay_active = false
end

local function cycle_sub_track()
    cancel_replay_timer(false)
    local list = get_sub_tracks()
    if #list == 0 then
        show_toast("No subtitle tracks")
        return
    end
    local cur_idx = 0
    if state.sub_visibility then
        for i, t in ipairs(list) do
            if t.selected then
                cur_idx = i
                break
            end
        end
    end
    if cur_idx == 0 then
        local t = list[1]
        mp.set_property_number("sid", t.id)
        mp.set_property_bool("sub-visibility", true)
        local lang = (t.lang and t.lang ~= "") and t.lang:upper() or ("Track " .. tostring(t.id))
        local title = (t.title and t.title ~= "") and (" · " .. truncate_text(t.title, 24)) or ""
        show_toast("Subtitles: " .. lang .. title)
    elseif cur_idx < #list then
        local t = list[cur_idx + 1]
        mp.set_property_number("sid", t.id)
        mp.set_property_bool("sub-visibility", true)
        local lang = (t.lang and t.lang ~= "") and t.lang:upper() or ("Track " .. tostring(t.id))
        local title = (t.title and t.title ~= "") and (" · " .. truncate_text(t.title, 24)) or ""
        show_toast("Subtitles: " .. lang .. title)
    else
        mp.set_property("sid", "no")
        show_toast("Subtitles: Off")
    end
end

local function find_english_sub_track()
    local list = get_sub_tracks()
    if #list == 0 then return nil end
    local fallback_eng = nil
    for _, t in ipairs(list) do
        local lang = (t.lang or ""):lower()
        local title = (t.title or ""):lower()
        local is_eng = (lang == "en" or lang == "eng" or lang:match("^en[-_]") or title:find("english"))
        if is_eng then
            if not t.forced and not title:find("forced") then
                return t
            elseif not fallback_eng then
                fallback_eng = t
            end
        end
    end
    return fallback_eng or list[1]
end

--------------------------------------------------------------------------------
-- Feature: Voice / Night Mode (dynaudnorm)
--------------------------------------------------------------------------------
local function has_night_filter()
    local af = mp.get_property_native("af") or {}
    for _, f in ipairs(af) do
        if f.label == NIGHT_FILTER_LABEL then
            return true, f.label
        end
        if f.name == "lavfi" and f.params and f.params.graph and tostring(f.params.graph):find("dynaudnorm") then
            return true, f.label
        end
    end
    return false, nil
end

local function set_night_mode(enabled, announce)
    if enabled then
        local exists = has_night_filter()
        if not exists then
            mp.commandv("af", "add", NIGHT_FILTER_SPEC)
        end
        state.night_active = true
        if announce then
            show_toast("Voice Night Mode: ON")
        end
    else
        local af = mp.get_property_native("af") or {}
        for _, f in ipairs(af) do
            if f.label == NIGHT_FILTER_LABEL then
                mp.commandv("af", "remove", "@" .. NIGHT_FILTER_LABEL)
            elseif f.name == "lavfi" and f.params and f.params.graph and tostring(f.params.graph):find("dynaudnorm") then
                if f.label and f.label ~= "" then
                    mp.commandv("af", "remove", "@" .. f.label)
                else
                    mp.commandv("af", "remove", "lavfi=[" .. tostring(f.params.graph) .. "]")
                end
            end
        end
        state.night_active = false
        if announce then
            show_toast("Voice Night Mode: OFF")
        end
    end
    render()
end

local function toggle_night_mode()
    set_night_mode(not state.night_active, true)
end

--------------------------------------------------------------------------------
-- Feature: GPU Neural Upscaling Shaders
--------------------------------------------------------------------------------
local function apply_shader_preset(preset_id, announce)
    preset_id = normalize_preset(preset_id)
    local preset = SHADER_PRESETS[preset_id]
    if not preset then return end

    if #preset.files == 0 then
        mp.commandv("change-list", "glsl-shaders", "clr", "")
        state.shader_preset = "off"
        if announce then show_toast(preset.toast) end
        render()
        return
    end

    local dir = resolve_shader_dir()
    local full_paths = {}
    local missing = {}
    for _, fn in ipairs(preset.files) do
        local p = dir .. "\\" .. fn
        if file_exists(p) then
            full_paths[#full_paths + 1] = p
        else
            missing[#missing + 1] = fn
        end
    end

    if #missing > 0 then
        msg.warn("Missing shader file(s) in " .. dir .. ": " .. table.concat(missing, ", "))
        show_toast("Missing shader: " .. missing[1], 3.0)
        return
    end

    mp.commandv("change-list", "glsl-shaders", "set", table.concat(full_paths, ";"))
    state.shader_preset = preset_id
    if announce then show_toast(preset.toast) end
    render()
end

local function cycle_shader_preset()
    local cur = state.shader_preset or "off"
    local next_idx = 1
    for i, id in ipairs(SHADER_PRESET_ORDER) do
        if id == cur then
            next_idx = (i % #SHADER_PRESET_ORDER) + 1
            break
        end
    end
    apply_shader_preset(SHADER_PRESET_ORDER[next_idx], true)
end

--------------------------------------------------------------------------------
-- Feature: Smart Black-Bar OLED Crop (cropdetect)
--------------------------------------------------------------------------------
local function remove_vf_label(label)
    local vf = mp.get_property_native("vf") or {}
    for _, f in ipairs(vf) do
        if f.label == label then
            mp.commandv("vf", "remove", "@" .. label)
            return true
        end
    end
    return false
end

local function on_cropdetect_meta(_, val)
    if state.crop_mode ~= "detecting" or not val then return end
    local cw = tonumber(val["lavfi.cropdetect.w"])
    local ch = tonumber(val["lavfi.cropdetect.h"])
    local cx = tonumber(val["lavfi.cropdetect.x"])
    local cy = tonumber(val["lavfi.cropdetect.y"])
    if not (cw and ch and cx and cy) or cw <= 0 or ch <= 0 then return end

    state.crop_samples = state.crop_samples + 1
    if not state.crop_detected then
        state.crop_detected = { w = cw, h = ch, x = cx, y = cy }
    else
        local x0 = math.min(state.crop_detected.x, cx)
        local y0 = math.min(state.crop_detected.y, cy)
        local x1 = math.max(state.crop_detected.x + state.crop_detected.w, cx + cw)
        local y1 = math.max(state.crop_detected.y + state.crop_detected.h, cy + ch)
        state.crop_detected = { w = x1 - x0, h = y1 - y0, x = x0, y = y0 }
    end
end

local function stop_cropdetect()
    if crop_timer then
        crop_timer:kill()
        crop_timer = nil
    end
    mp.unobserve_property(on_cropdetect_meta)
    remove_vf_label(CROPDETECT_LABEL)
end

local function clear_crop(announce)
    stop_cropdetect()
    remove_vf_label(CROP_LABEL)
    state.crop_mode = "off"
    state.crop_detected = nil
    state.crop_samples = 0
    if announce then show_toast("Black-Bar Crop: Off") end
    render()
end

local function finish_cropdetect()
    stop_cropdetect()
    local det = state.crop_detected
    local vw = state.vid_w
    local vh = state.vid_h
    if not det or vw <= 0 or vh <= 0 then
        state.crop_mode = "off"
        show_toast(state.paused and "Unpause briefly to detect bars" or "Crop: Unable to detect bars")
        render()
        return
    end

    local diff_w = vw - det.w
    local diff_h = vh - det.h
    local sane_size = (det.w >= vw * 0.45) and (det.h >= vh * 0.45)
    if sane_size and (diff_w >= 8 or diff_h >= 8) then
        local cw = math.floor(det.w / 2) * 2
        local ch = math.floor(det.h / 2) * 2
        local cx = math.floor(det.x / 2) * 2
        local cy = math.floor(det.y / 2) * 2
        remove_vf_label(CROP_LABEL)
        mp.commandv("vf", "add", string.format("@%s:lavfi=[crop=%d:%d:%d:%d]", CROP_LABEL, cw, ch, cx, cy))
        state.crop_mode = "active"
        show_toast(string.format("OLED Black-Bar Crop: %dx%d", cw, ch))
    else
        state.crop_mode = "off"
        show_toast("No black bars detected")
    end
    render()
end

local function toggle_smart_crop()
    if state.crop_mode == "active" or state.crop_mode == "detecting" then
        clear_crop(true)
        return
    end

    local vw = mp.get_property_number("width", 0) or 0
    local vh = mp.get_property_number("height", 0) or 0
    state.vid_w = vw
    state.vid_h = vh
    if vw <= 0 or vh <= 0 then
        show_toast("No video active")
        return
    end

    state.crop_mode = "detecting"
    state.crop_detected = nil
    state.crop_samples = 0
    remove_vf_label(CROP_LABEL)
    remove_vf_label(CROPDETECT_LABEL)
    mp.observe_property("vf-metadata/" .. CROPDETECT_LABEL, "native", on_cropdetect_meta)
    mp.commandv("vf", "add", "@" .. CROPDETECT_LABEL .. ":lavfi=[cropdetect=limit=24:round=2:reset=0]")
    show_toast("Detecting black bars...", 1.2)
    crop_timer = mp.add_timeout(0.85, finish_cropdetect)
    render()
end

--------------------------------------------------------------------------------
-- Feature: "What did they just say?" 8s Replay with Temporary Subtitles
--------------------------------------------------------------------------------
local function trigger_replay()
    if opts.is_live then
        show_toast("Replay unavailable on Live TV")
        return
    end

    if not state.replay_active then
        state.replay_prev_sid = mp.get_property("sid") or "no"
        state.replay_prev_vis = mp.get_property_bool("sub-visibility", true)
    end

    mp.commandv("seek", tostring(-REPLAY_SEEK_SECS), "relative+exact")

    local subs_were_off = (state.replay_prev_sid == "no" or state.replay_prev_sid == false or not state.replay_prev_vis)
    if subs_were_off then
        local best = find_english_sub_track()
        if best then
            mp.set_property_number("sid", best.id)
            mp.set_property_bool("sub-visibility", true)
        end
    end

    state.replay_active = true
    show_toast("Replaying 8s with subtitles", 2.6)

    if replay_timer then
        replay_timer:kill()
        replay_timer = nil
    end
    replay_timer = mp.add_timeout(REPLAY_RESTORE_SECS, function()
        replay_timer = nil
        cancel_replay_timer(true)
        render()
    end)
    render()
end

--------------------------------------------------------------------------------
-- Feature: Smart Skip (Recap / Intro / Skip Start) & Smart Skip Credits / Up Next
--------------------------------------------------------------------------------
local function get_skip_start_target(pos, dur)
    local ch2 = state.chapters and state.chapters[2]
    if ch2 and ch2.time and ch2.time >= 25 and ch2.time <= 210 and ch2.time > (pos + 8) then
        local t = ch2.time
        if dur > 0 and t > dur - 1 then t = math.max(0, dur - 1) end
        return t, true
    end
    local target = pos + 85
    if dur > 0 then
        target = math.min(target, math.max(0, dur - 1))
    end
    return target, false
end

local function do_skip_recap(auto)
    if opts.recap_e > 0 then
        state.recap_dismissed = true
        state.skip_pill_mode = nil
        mp.set_property_number("time-pos", opts.recap_e)
        show_toast(auto and "Auto-Skipped Recap" or "Skipped Recap")
    end
end

local function do_skip_intro(auto)
    if opts.intro_e > 0 then
        state.intro_dismissed = true
        state.skip_pill_mode = nil
        mp.set_property_number("time-pos", opts.intro_e)
        show_toast(auto and "Auto-Skipped Intro" or "Skipped Intro")
    end
end

local function do_skip_start()
    local pos = state.time_pos or 0
    local dur = state.duration or 0
    local target, is_ch = get_skip_start_target(pos, dur)
    if target and target > pos + 1 then
        state.skip_start_dismissed = true
        state.skip_pill_mode = nil
        mp.set_property_number("time-pos", target)
        show_toast(is_ch and "Skipped Start" or "Skipped Start (+85s)")
    end
end

local function dismiss_up_next()
    state.up_next_dismissed = true
    state.up_next_visible = false
    state.up_next_last_tick = nil
    set_up_next_enter_bound(false)
    render()
end

local function dismiss_credits_exit()
    state.credits_exit_dismissed = true
    state.credits_exit_visible = false
    render()
end

local function update_binge_state()
    if opts.is_live then
        state.skip_pill_mode = nil
        state.skip_pill_label = nil
        state.skip_pill_target = nil
        state.up_next_visible = false
        state.credits_exit_visible = false
        set_up_next_enter_bound(false)
        return
    end

    local pos = state.time_pos or 0
    local dur = state.duration or 0

    ----------------------------------------------------------------------------
    -- 1. Smart Skip Credits & Automatic Up Next (or Credits Exit Pill)
    ----------------------------------------------------------------------------
    local countdown_secs = opts.autoskip_credits and 5 or 10

    if opts.has_next and not state.quitting_for_next and dur > 120 then
        state.credits_exit_visible = false
        local trigger
        if opts.credits >= 0 and opts.credits < dur - 3 then
            trigger = opts.credits
        elseif opts.is_series then
            trigger = (dur >= 2400) and (dur - 120)
                or ((dur >= 1200) and (dur - 90) or math.max(0, dur - 45))
        else
            trigger = math.max(0, dur - 40)
        end

        if trigger ~= nil and pos >= trigger then
            if not state.up_next_dismissed then
                local now = mp.get_time()
                if not state.up_next_visible then
                    state.up_next_visible = true
                    state.up_next_total = countdown_secs
                    state.up_next_remaining = countdown_secs
                    state.up_next_last_tick = now
                elseif opts.autoplay then
                    if state.paused then
                        state.up_next_last_tick = now
                    else
                        local dt = math.max(0, now - (state.up_next_last_tick or now))
                        state.up_next_last_tick = now
                        state.up_next_remaining = math.max(0, (state.up_next_remaining or countdown_secs) - dt)
                        if state.up_next_remaining <= 0 then
                            trigger_play_next()
                            return
                        end
                    end
                end
            else
                state.up_next_visible = false
            end
        else
            if state.up_next_visible or state.up_next_dismissed then
                state.up_next_visible = false
                state.up_next_dismissed = false
                state.up_next_total = countdown_secs
                state.up_next_remaining = countdown_secs
                state.up_next_last_tick = nil
            end
        end
    else
        state.up_next_visible = false

        if not opts.has_next and not state.quitting_for_next and opts.credits >= 0 and dur > 0 and pos >= opts.credits then
            state.credits_exit_visible = not state.credits_exit_dismissed
        else
            state.credits_exit_visible = false
            if opts.credits >= 0 and pos < opts.credits then
                state.credits_exit_dismissed = false
            end
        end
    end

    set_up_next_enter_bound(state.up_next_visible)

    ----------------------------------------------------------------------------
    -- 2. Floating Skip Pill priority: Recap > Intro > Smart Skip Start
    ----------------------------------------------------------------------------
    local has_recap = (opts.recap_s >= 0 and opts.recap_e > opts.recap_s and (opts.recap_e - 2) >= opts.recap_s)
    local has_intro = (opts.intro_s >= 0 and opts.intro_e > opts.intro_s and (opts.intro_e - 2) >= opts.intro_s)

    if has_recap and pos < opts.recap_s then
        state.recap_dismissed = false
    end
    if has_intro and pos < opts.intro_s then
        state.intro_dismissed = false
    end

    state.skip_pill_mode = nil
    state.skip_pill_label = nil
    state.skip_pill_target = nil

    if has_recap and pos >= opts.recap_s and pos <= (opts.recap_e - 2) then
        if opts.autoskip and not state.recap_autoskipped then
            state.recap_autoskipped = true
            do_skip_recap(true)
            return
        elseif not state.recap_dismissed then
            state.skip_pill_mode = "recap"
            state.skip_pill_label = "Skip Recap ›"
            state.skip_pill_target = opts.recap_e
        end
    elseif has_intro and pos >= opts.intro_s and pos <= (opts.intro_e - 2) then
        if opts.autoskip and not state.intro_autoskipped then
            state.intro_autoskipped = true
            do_skip_intro(true)
            return
        elseif not state.intro_dismissed then
            state.skip_pill_mode = "intro"
            state.skip_pill_label = "Skip Intro ›"
            state.skip_pill_target = opts.intro_e
        end
    elseif opts.is_series and not has_recap and not has_intro
        and not state.up_next_visible and not state.credits_exit_visible
        and pos >= 2 and pos <= 165 and not state.skip_start_dismissed then
        local target, is_ch = get_skip_start_target(pos, dur)
        if target and target > pos + 2 then
            state.skip_pill_mode = "start"
            state.skip_pill_label = is_ch and "Skip Start ›" or "Skip Start (+85s) ›"
            state.skip_pill_target = target
        end
    end
end

local function handle_smart_skip()
    if opts.is_live then
        show_toast("Skip unavailable on Live TV")
        return
    end
    update_binge_state()

    if state.up_next_visible and opts.has_next then
        trigger_play_next()
        return
    end

    local pos = state.time_pos or 0
    local has_recap = (opts.recap_s >= 0 and opts.recap_e > opts.recap_s and (opts.recap_e - 2) >= opts.recap_s)
    local has_intro = (opts.intro_s >= 0 and opts.intro_e > opts.intro_s and (opts.intro_e - 2) >= opts.intro_s)

    if state.skip_pill_mode == "recap" or (has_recap and pos >= opts.recap_s and pos <= opts.recap_e - 1) then
        do_skip_recap(false)
        return
    end

    if state.skip_pill_mode == "intro" or (has_intro and pos >= opts.intro_s and pos <= opts.intro_e - 1) then
        do_skip_intro(false)
        return
    end

    if state.skip_pill_mode == "start"
        or (opts.is_series and not has_recap and not has_intro and pos >= 0 and pos <= 180) then
        do_skip_start()
        return
    end

    show_toast("Nothing to skip right now", 1.6)
end

--------------------------------------------------------------------------------
-- HUD Visibility & Auto-Hide (2.5s mouse inactivity; always shown on pause / drawer)
--------------------------------------------------------------------------------
schedule_hide = function()
    if hide_timer then
        hide_timer:kill()
        hide_timer = nil
    end
    hide_timer = mp.add_timeout(HIDE_DELAY, function()
        hide_timer = nil
        if not state.paused and not state.dragging_seek and not state.dragging_vol and not state.ep_drawer_open then
            state.hud_visible = false
            render()
        end
    end)
end

local function wake_hud()
    state.hud_visible = true
    schedule_hide()
    render()
end

--------------------------------------------------------------------------------
-- ASS Drawing Primitives
--------------------------------------------------------------------------------
local function ass_shape(color, alpha, draw_fn, bord, bord_color, bord_alpha)
    local a = assdraw.ass_new()
    a:draw_start()
    draw_fn(a)
    a:draw_stop()
    local b_tag = ""
    if bord and bord > 0 then
        b_tag = string.format("\\bord%.1f\\3c%s\\3a%s", bord, bord_color or C.border, bord_alpha or "&H00&")
    else
        b_tag = "\\bord0"
    end
    return string.format("{\\an7\\pos(0,0)%s\\shad0\\1c%s\\1a%s\\p1}%s{\\p0}", b_tag, color, alpha or "&H00&", a.text)
end

local function ass_rect(x0, y0, x1, y1, color, alpha, radius, bord, bord_color, bord_alpha)
    return ass_shape(color, alpha, function(a)
        if radius and radius > 0 then
            a:round_rect_cw(x0, y0, x1, y1, radius)
        else
            a:rect_cw(x0, y0, x1, y1)
        end
    end, bord, bord_color, bord_alpha)
end

local function ass_circle(cx, cy, r, color, alpha, bord, bord_color)
    return ass_rect(cx - r, cy - r, cx + r, cy + r, color, alpha, r, bord, bord_color)
end

local function ass_text(x, y, an, size, color, text, bold, alpha)
    return string.format(
        "{\\an%d\\pos(%.1f,%.1f)\\fn%s\\fs%d\\b%d\\bord0\\shad0\\1c%s\\1a%s}%s",
        an, x, y, FONT, size, bold and 1 or 0, color, alpha or "&H00&", ass_escape(text)
    )
end

local function draw_play_icon(cx, cy, size, color)
    local h = size * 0.5
    local w = size * 0.44
    return ass_shape(color, "&H00&", function(a)
        a:move_to(cx - w * 0.7, cy - h)
        a:line_to(cx + w * 1.15, cy)
        a:line_to(cx - w * 0.7, cy + h)
    end)
end

local function draw_pause_icon(cx, cy, size, color)
    local h = size * 0.48
    local bw = size * 0.20
    local gap = size * 0.14
    return ass_shape(color, "&H00&", function(a)
        a:rect_cw(cx - gap - bw, cy - h, cx - gap, cy + h)
        a:rect_cw(cx + gap, cy - h, cx + gap + bw, cy + h)
    end)
end

local function draw_speaker_icon(cx, cy, size, color, muted)
    local s = size * 0.5
    local lines = {}
    lines[#lines + 1] = ass_shape(color, "&H00&", function(a)
        a:move_to(cx - s * 0.85, cy - s * 0.34)
        a:line_to(cx - s * 0.32, cy - s * 0.34)
        a:line_to(cx + s * 0.22, cy - s * 0.78)
        a:line_to(cx + s * 0.22, cy + s * 0.78)
        a:line_to(cx - s * 0.32, cy + s * 0.34)
        a:line_to(cx - s * 0.85, cy + s * 0.34)
    end)
    if muted then
        lines[#lines + 1] = ass_shape(C.red, "&H00&", function(a)
            local x0, y0 = cx + s * 0.42, cy - s * 0.42
            local x1, y1 = cx + s * 1.02, cy + s * 0.42
            local t = 2.0
            a:move_to(x0, y0 - t) a:line_to(x1 + t, y1) a:line_to(x1, y1 + t) a:line_to(x0 - t, y0)
            a:move_to(x1, y0 - t) a:line_to(x1 + t, y0) a:line_to(x0, y1 + t) a:line_to(x0 - t, y1)
        end)
    else
        lines[#lines + 1] = ass_shape(color, "&H00&", function(a)
            a:round_rect_cw(cx + s * 0.46, cy - s * 0.36, cx + s * 0.64, cy + s * 0.36, 1.5)
            a:round_rect_cw(cx + s * 0.78, cy - s * 0.62, cx + s * 0.96, cy + s * 0.62, 1.5)
        end)
    end
    return table.concat(lines, "\n")
end

--------------------------------------------------------------------------------
-- Modular Render Sections
--------------------------------------------------------------------------------
local function render_top_bar(ctx)
    local W, pad = ctx.W, ctx.pad
    local push = ctx.push
    local top_h = 98
    state.top_bar_y1 = top_h

    -- Pure-black OLED backdrop with smooth 4-step edge feather
    push(ass_rect(0, 0, W, top_h, C.black, "&H12&"))
    push(ass_rect(0, top_h, W, top_h + 14, C.black, "&H48&"))
    push(ass_rect(0, top_h + 14, W, top_h + 30, C.black, "&H96&"))
    push(ass_rect(0, top_h + 30, W, top_h + 46, C.black, "&HD0&"))
    push(ass_rect(0, top_h - 1, W, top_h, C.border, "&HA0&"))

    -- Top-Left: Live badge (if live) + Title + Subtitle
    local title_x = pad
    local display_title = (opts.title ~= "") and opts.title
        or (state.media_title ~= "" and state.media_title or "Now Playing")
    if opts.is_live then
        local badge_w = 88
        local by0, by1 = 24, 56
        push(ass_rect(title_x, by0, title_x + badge_w, by1, C.red, "&H00&", 6))
        push(ass_circle(title_x + 18, (by0 + by1) * 0.5, 4.5, C.white, "&H00&"))
        push(ass_text(title_x + 30, (by0 + by1) * 0.5, 4, 18, C.white, "LIVE", true))
        title_x = title_x + badge_w + 16
    end

    local right_edge = state.ep_drawer_open and (W - ctx.drawer_w - 24) or (W - pad)
    local title_avail = math.max(240, right_edge - title_x - 280)
    push(ass_text(title_x, 20, 7, 34, C.white, truncate_to_width(display_title, 34, title_avail), true))

    local sub_text = opts.sub
    if sub_text == "" and state.vid_w > 0 and state.vid_h > 0 then
        sub_text = string.format("%dx%d", state.vid_w, state.vid_h)
        if state.shader_preset ~= "off" then
            sub_text = sub_text .. "  ·  " .. SHADER_PRESETS[state.shader_preset].pill
        end
    end
    if sub_text ~= "" then
        local sub_avail = math.max(260, right_edge - pad - 280)
        push(ass_text(pad, 62, 7, 21, C.muted, truncate_to_width(sub_text, 21, sub_avail), false))
    end

    -- Top-Right: Current Wall Clock + Ends-at / Remaining (shifts left if drawer open)
    local epoch_now = os.time()
    push(ass_text(right_edge, 20, 9, 30, C.white, fmt_clock(epoch_now), true))

    local meta_right = ""
    if opts.is_live then
        meta_right = "LIVE STREAM"
        if state.time_pos > 1 then
            meta_right = meta_right .. "  ·  " .. fmt_time(state.time_pos)
        end
    elseif state.duration > 0 then
        local rem = math.max(0, state.duration - state.time_pos)
        local eff_rem = rem / math.max(0.1, state.speed or 1.0)
        local ends_str = fmt_clock(epoch_now + math.floor(eff_rem + 0.5))
        meta_right = "Ends " .. ends_str .. "  ·  -" .. fmt_time(rem)
    end
    if meta_right ~= "" then
        push(ass_text(right_edge, 62, 9, 20, C.muted, meta_right, false))
    end
end

local function render_bottom_deck(ctx)
    local W, H, pad = ctx.W, ctx.H, ctx.pad
    local mx = ctx.mx
    local push, register_hit, is_hovered = ctx.push, ctx.register_hit, ctx.is_hovered

    local deck_h = 166
    local deck_y0 = H - deck_h
    state.bot_bar_y0 = deck_y0

    push(ass_rect(0, deck_y0 - 44, W, deck_y0 - 28, C.black, "&HD4&"))
    push(ass_rect(0, deck_y0 - 28, W, deck_y0 - 12, C.black, "&H92&"))
    push(ass_rect(0, deck_y0 - 12, W, deck_y0, C.black, "&H44&"))
    push(ass_rect(0, deck_y0, W, H, C.black, "&H0E&"))
    push(ass_rect(0, deck_y0, W, deck_y0 + 1, C.border, "&HA0&"))

    ----------------------------------------------------------------------------
    -- Row 1: Seek Bar
    ----------------------------------------------------------------------------
    local bar_x0 = pad
    local bar_x1 = W - pad
    local bar_w = bar_x1 - bar_x0
    local bar_cy = H - 124
    local dur = state.duration or 0
    local pos = state.dragging_seek and (state.seek_drag_time or state.time_pos) or state.time_pos

    if opts.is_live or dur <= 0 then
        push(ass_rect(bar_x0, bar_cy - 3, bar_x1, bar_cy + 3, C.red, "&H00&", 3))
        push(ass_circle(bar_x1, bar_cy, 6, C.red, "&H00&"))
        push(ass_text(bar_x0, bar_cy + 14, 7, 18, C.red, "● LIVE", true))
        if state.time_pos > 0 then
            push(ass_text(bar_x1, bar_cy + 14, 9, 18, C.muted, fmt_time(state.time_pos), false))
        end
    else
        local seek_hit = { x0 = bar_x0 - 10, y0 = bar_cy - 22, x1 = bar_x1 + 10, y1 = bar_cy + 22 }
        state.seek_box = { x0 = bar_x0, x1 = bar_x1, y0 = seek_hit.y0, y1 = seek_hit.y1 }
        local seek_hovered = state.dragging_seek or is_hovered(seek_hit.x0, seek_hit.y0, seek_hit.x1, seek_hit.y1)
        local track_h = seek_hovered and 10 or 6
        local ty0 = bar_cy - track_h * 0.5
        local ty1 = bar_cy + track_h * 0.5
        local tr = track_h * 0.5

        -- Background rail
        push(ass_rect(bar_x0, ty0, bar_x1, ty1, C.track_bg, "&H00&", tr))

        -- Buffered/cached segment
        if state.cache_time and state.cache_time > pos then
            local cache_frac = clamp(state.cache_time / dur, 0, 1)
            local cx1 = bar_x0 + bar_w * cache_frac
            if cx1 > bar_x0 + 2 then
                push(ass_rect(bar_x0, ty0, cx1, ty1, C.track_cache, "&H00&", tr))
            end
        end

        -- Recap range highlight (Violet #A87CF5)
        if opts.recap_s >= 0 and opts.recap_e > opts.recap_s then
            local rx0 = bar_x0 + bar_w * clamp(opts.recap_s / dur, 0, 1)
            local rx1 = bar_x0 + bar_w * clamp(opts.recap_e / dur, 0, 1)
            if rx1 - rx0 < 4 then rx1 = math.min(bar_x1, rx0 + 4) end
            push(ass_rect(rx0, ty0 - 1, rx1, ty1 + 1, C.purple, "&H36&", 2))
            push(ass_rect(rx0 - 1, ty0 - 4, rx0 + 2, ty1 + 4, C.purple, "&H00&", 1))
            push(ass_rect(rx1 - 1, ty0 - 4, rx1 + 2, ty1 + 4, C.purple, "&H00&", 1))
        end

        -- Intro range highlight (Cyan #00D4FF)
        if opts.intro_s >= 0 and opts.intro_e > opts.intro_s then
            local ix0 = bar_x0 + bar_w * clamp(opts.intro_s / dur, 0, 1)
            local ix1 = bar_x0 + bar_w * clamp(opts.intro_e / dur, 0, 1)
            if ix1 - ix0 < 4 then ix1 = math.min(bar_x1, ix0 + 4) end
            push(ass_rect(ix0, ty0 - 1, ix1, ty1 + 1, C.cyan, "&H30&", 2))
            push(ass_rect(ix0 - 1, ty0 - 4, ix0 + 2, ty1 + 4, C.cyan, "&H00&", 1))
            push(ass_rect(ix1 - 1, ty0 - 4, ix1 + 2, ty1 + 4, C.cyan, "&H00&", 1))
        end

        -- Credits marker highlight (Amber #FFB300)
        if opts.credits >= 0 and opts.credits < dur then
            local crx = bar_x0 + bar_w * clamp(opts.credits / dur, 0, 1)
            push(ass_rect(crx, ty0 - 1, bar_x1, ty1 + 1, C.amber, "&H45&", 2))
            push(ass_rect(crx - 1.5, ty0 - 5, crx + 1.5, ty1 + 5, C.amber, "&H00&", 1))
        end

        -- Played progress (Netflix Red #E50914)
        local prog_frac = clamp(pos / dur, 0, 1)
        local px = bar_x0 + bar_w * prog_frac
        if px > bar_x0 + 2 then
            push(ass_rect(bar_x0, ty0, px, ty1, C.red, "&H00&", tr))
        end

        -- Subtle native chapter tick marks
        if state.chapters and #state.chapters > 1 then
            for _, ch in ipairs(state.chapters) do
                if ch.time > 1 and ch.time < (dur - 1) then
                    local chx = bar_x0 + bar_w * clamp(ch.time / dur, 0, 1)
                    push(ass_rect(chx - 1, ty0 - 2, chx + 1, ty1 + 2, C.white, "&H80&"))
                end
            end
        end

        -- Playhead knob
        local knob_r = seek_hovered and 11 or 7.5
        push(ass_circle(px, bar_cy, knob_r, C.red, "&H00&", 1.5, C.white))

        -- Hover timestamp tooltip + hairline
        if seek_hovered then
            local hx = clamp(mx, bar_x0, bar_x1)
            local h_frac = clamp((hx - bar_x0) / bar_w, 0, 1)
            local h_time = h_frac * dur
            local tip_str = fmt_time(h_time)
            local tip_color = C.white
            if opts.recap_s >= 0 and opts.recap_e > opts.recap_s and h_time >= opts.recap_s and h_time <= opts.recap_e then
                tip_str = tip_str .. " · Recap"
                tip_color = C.purple
            elseif opts.intro_s >= 0 and opts.intro_e > opts.intro_s and h_time >= opts.intro_s and h_time <= opts.intro_e then
                tip_str = tip_str .. " · Intro"
                tip_color = C.cyan
            elseif opts.credits >= 0 and h_time >= opts.credits then
                tip_str = tip_str .. " · Credits"
                tip_color = C.amber
            else
                local ch = find_chapter_at(h_time)
                if ch and ch.title ~= "" then
                    tip_str = tip_str .. " · " .. truncate_text(ch.title, 26)
                end
            end

            push(ass_rect(hx - 1, ty0 - 5, hx + 1, ty1 + 5, C.white, "&H30&"))
            local tip_w = estimate_text_w(tip_str, 19) + 26
            local tip_h = 34
            local tip_cx = clamp(hx, bar_x0 + tip_w * 0.5, bar_x1 - tip_w * 0.5)
            local tip_y1 = bar_cy - 18
            local tip_y0 = tip_y1 - tip_h
            push(ass_rect(tip_cx - tip_w * 0.5, tip_y0, tip_cx + tip_w * 0.5, tip_y1, C.black, "&H08&", 7, 1.5, C.border_hi))
            push(ass_text(tip_cx, (tip_y0 + tip_y1) * 0.5, 5, 19, tip_color, tip_str, true))
        end

        -- Elapsed / Duration labels
        push(ass_text(bar_x0, bar_cy + 14, 7, 18, C.text, fmt_time(pos), false))
        push(ass_text(bar_x1, bar_cy + 14, 9, 18, C.muted, fmt_time(dur), false))
    end

    ----------------------------------------------------------------------------
    -- Row 2: Left Transport Controls + Prev / Next Episode Buttons
    ----------------------------------------------------------------------------
    local ctrl_cy = H - 52
    local btn_h = 40
    local by0 = ctrl_cy - btn_h * 0.5
    local by1 = ctrl_cy + btn_h * 0.5
    local tight_left = (W < 1520) or ((opts.has_prev and opts.has_next) and W < 1720)
    local lx = pad
    local btn_tooltip = nil

    -- 1) Play / Pause
    local pp_w = 46
    local pp_hov = is_hovered(lx, by0, lx + pp_w, by1)
    push(ass_rect(lx, by0, lx + pp_w, by1, pp_hov and C.red or C.pill, "&H00&", 10, 1.2, pp_hov and C.red_hover or C.border))
    if state.paused then
        push(draw_play_icon(lx + pp_w * 0.5 + 1, ctrl_cy, 19, C.white))
    else
        push(draw_pause_icon(lx + pp_w * 0.5, ctrl_cy, 19, C.white))
    end
    register_hit("play_pause", lx, by0, lx + pp_w, by1, function()
        mp.commandv("cycle", "pause")
    end)
    lx = lx + pp_w + (tight_left and 8 or 10)

    -- 2) -10s & +10s & Prev & Next (VOD only)
    if not opts.is_live then
        local sk_w = tight_left and 52 or 56
        local sk_fs = tight_left and 16 or 18
        local back_hov = is_hovered(lx, by0, lx + sk_w, by1)
        push(ass_rect(lx, by0, lx + sk_w, by1, back_hov and C.pill_hover or C.pill, "&H00&", 10, 1.2, back_hov and C.border_hi or C.border))
        push(ass_text(lx + sk_w * 0.5, ctrl_cy, 5, sk_fs, C.text, "-10s", true))
        register_hit("seek_back", lx, by0, lx + sk_w, by1, function()
            mp.commandv("seek", "-10", "relative")
        end)
        lx = lx + sk_w + (tight_left and 6 or 8)

        local fwd_hov = is_hovered(lx, by0, lx + sk_w, by1)
        push(ass_rect(lx, by0, lx + sk_w, by1, fwd_hov and C.pill_hover or C.pill, "&H00&", 10, 1.2, fwd_hov and C.border_hi or C.border))
        push(ass_text(lx + sk_w * 0.5, ctrl_cy, 5, sk_fs, C.text, "+10s", true))
        register_hit("seek_fwd", lx, by0, lx + sk_w, by1, function()
            mp.commandv("seek", "10", "relative")
        end)
        lx = lx + sk_w + (tight_left and 6 or 8)

        -- Prev Episode pill (|< Prev)
        if opts.has_prev then
            local prev_lbl = tight_left and "|<" or "|< Prev"
            local prev_w = tight_left and 46 or 78
            local prev_hov = is_hovered(lx, by0, lx + prev_w, by1)
            push(ass_rect(lx, by0, lx + prev_w, by1, prev_hov and C.pill_hover or C.pill, "&H00&", 10, 1.2, prev_hov and C.border_hi or C.border))
            push(ass_text(lx + prev_w * 0.5, ctrl_cy, 5, tight_left and 16 or 17, prev_hov and C.white or C.text, prev_lbl, true))
            register_hit("ep_prev", lx, by0, lx + prev_w, by1, trigger_play_prev)
            if prev_hov and opts.prev_label ~= "" then
                btn_tooltip = { cx = lx + prev_w * 0.5, text = "Prev · " .. opts.prev_label }
            end
            lx = lx + prev_w + (tight_left and 6 or 8)
        end

        -- Next Episode pill (Next >|)
        if opts.has_next then
            local next_lbl = tight_left and "Next" or "Next >|"
            local next_w = tight_left and 58 or 82
            local next_hov = is_hovered(lx, by0, lx + next_w, by1)
            local n_fill = next_hov and C.red_dim or C.pill
            local n_bord = next_hov and C.red or C.border
            push(ass_rect(lx, by0, lx + next_w, by1, n_fill, "&H00&", 10, 1.3, n_bord))
            push(ass_text(lx + next_w * 0.5, ctrl_cy, 5, tight_left and 16 or 17, next_hov and C.white or C.text, next_lbl, true))
            register_hit("ep_next", lx, by0, lx + next_w, by1, trigger_play_next)
            if next_hov and opts.next_label ~= "" then
                btn_tooltip = { cx = lx + next_w * 0.5, text = "Next · " .. opts.next_label }
            end
            lx = lx + next_w + (tight_left and 8 or 12)
        else
            lx = lx + 4
        end
    end

    -- 3) Mute button
    local mute_w = 42
    local mute_hov = is_hovered(lx, by0, lx + mute_w, by1)
    push(ass_rect(lx, by0, lx + mute_w, by1, mute_hov and C.pill_hover or C.pill, "&H00&", 10, 1.2, mute_hov and C.border_hi or C.border))
    push(draw_speaker_icon(lx + mute_w * 0.5, ctrl_cy, 18, state.mute and C.muted or C.white, state.mute))
    register_hit("mute", lx, by0, lx + mute_w, by1, function()
        mp.commandv("cycle", "mute")
    end)
    lx = lx + mute_w + (tight_left and 8 or 10)

    -- 4) Volume slider + numeric %
    local vol_w = tight_left and 78 or 96
    local vx0, vx1 = lx, lx + vol_w
    local vol_hit = { x0 = vx0 - 4, y0 = by0, x1 = vx1 + 4, y1 = by1 }
    state.vol_box = { x0 = vx0, x1 = vx1, y0 = by0, y1 = by1 }
    local vol_hov = state.dragging_vol or is_hovered(vol_hit.x0, vol_hit.y0, vol_hit.x1, vol_hit.y1)
    local vol_frac = state.mute and 0 or clamp((state.volume or 100) / 100, 0, 1)
    local v_fill_x = vx0 + vol_w * vol_frac
    push(ass_rect(vx0, ctrl_cy - 3, vx1, ctrl_cy + 3, C.track_bg, "&H00&", 3))
    if v_fill_x > vx0 + 2 then
        push(ass_rect(vx0, ctrl_cy - 3, v_fill_x, ctrl_cy + 3, state.mute and C.dim or C.red, "&H00&", 3))
    end
    push(ass_circle(v_fill_x, ctrl_cy, vol_hov and 8 or 6, state.mute and C.muted or C.white, "&H00&"))
    lx = vx1 + 10
    local vol_label = state.mute and "Mute" or string.format("%d%%", math.floor((state.volume or 0) + 0.5))
    push(ass_text(lx, ctrl_cy, 4, 17, C.muted, vol_label, false))
    local left_end_x = lx + estimate_text_w(vol_label, 17) + 16

    ----------------------------------------------------------------------------
    -- Row 2: Right Compact Feature Pills (right-to-left, auto-fitting cleanly)
    ----------------------------------------------------------------------------
    local crop_label = (state.crop_mode == "active") and "Crop: ON"
        or (state.crop_mode == "detecting" and "Crop..." or "Crop")
    local voice_label = state.night_active and "Voice: ON" or "Voice"
    local replay_label = state.replay_active and "Replay: ON" or "Replay"
    local shader_label = SHADER_PRESETS[state.shader_preset].pill
    local sub_lbl = get_sub_pill_label()

    -- Visual order left-to-right: Audio | Sub | Voice | Shader | Crop | Replay | [Episodes]
    local pills = {
        { id = "audio",  label = get_audio_pill_label(), active = false, action = cycle_audio_track },
        { id = "sub",    label = sub_lbl, active = (sub_lbl ~= "Sub: Off" and sub_lbl ~= "Sub: None"), action = cycle_sub_track },
        { id = "voice",  label = voice_label, active = state.night_active, action = toggle_night_mode },
        { id = "shader", label = shader_label, active = (state.shader_preset ~= "off"), action = cycle_shader_preset },
        { id = "crop",   label = crop_label, active = (state.crop_mode ~= "off"), action = toggle_smart_crop },
        { id = "replay", label = replay_label, active = state.replay_active, action = trigger_replay },
    }
    if #state.episodes >= 2 then
        pills[#pills + 1] = {
            id = "episodes",
            label = "Episodes",
            active = state.ep_drawer_open,
            action = toggle_ep_drawer,
        }
    end

    -- Auto-compact pills when window is narrow or many pills are active so nothing ever overlaps
    local compact = (W < 1520)
    local pill_fs = compact and 16 or 18
    local pill_pad = compact and 16 or 22
    local pill_gap = compact and 6 or 9
    local min_pw = compact and 64 or 72

    local function measure_pills(fs, pp, pg, mpw)
        local total = 0
        for i, p in ipairs(pills) do
            total = total + math.max(mpw, estimate_text_w(p.label, fs) + pp)
            if i < #pills then total = total + pg end
        end
        return total
    end

    local avail_right = math.max(300, (W - pad) - left_end_x)
    if measure_pills(pill_fs, pill_pad, pill_gap, min_pw) > avail_right then
        pill_fs, pill_pad, pill_gap, min_pw = 16, 16, 6, 62
    end
    if measure_pills(pill_fs, pill_pad, pill_gap, min_pw) > avail_right then
        pill_fs, pill_pad, pill_gap, min_pw = 15, 12, 5, 54
    end

    local rx = W - pad
    for i = #pills, 1, -1 do
        local p = pills[i]
        local pw = math.max(min_pw, estimate_text_w(p.label, pill_fs) + pill_pad)
        local px1 = rx
        local px0 = rx - pw
        local hov = is_hovered(px0, by0, px1, by1)

        local fill, bord_c, txt_c
        if p.active and p.id ~= "sub" then
            fill = hov and C.red_hover or C.red_dim
            bord_c = C.red
            txt_c = C.white
        elseif hov then
            fill = C.pill_hover
            bord_c = C.border_hi
            txt_c = C.white
        else
            fill = C.pill
            bord_c = p.active and C.border_hi or C.border
            txt_c = p.active and C.white or C.text
        end

        push(ass_rect(px0, by0, px1, by1, fill, "&H00&", 10, 1.3, bord_c))
        push(ass_text((px0 + px1) * 0.5, ctrl_cy, 5, pill_fs, txt_c, p.label, p.active or hov))
        register_hit("pill_" .. p.id, px0, by0, px1, by1, p.action)

        rx = px0 - pill_gap
    end

    -- Hover tooltip for Prev / Next episode buttons
    if btn_tooltip then
        local txt = truncate_text(btn_tooltip.text, 38)
        local tw = estimate_text_w(txt, 17) + 24
        local th = 32
        local tcx = clamp(btn_tooltip.cx, pad + tw * 0.5, W - pad - tw * 0.5)
        local ty1 = by0 - 10
        local ty0 = ty1 - th
        push(ass_rect(tcx - tw * 0.5, ty0, tcx + tw * 0.5, ty1, C.black, "&H06&", 7, 1.4, C.border_hi))
        push(ass_text(tcx, (ty0 + ty1) * 0.5, 5, 17, C.white, txt, false))
    end
end

local function render_skip_pill(ctx)
    local mode = state.skip_pill_mode
    if not mode then return end

    local H = ctx.H
    local push, register_hit, is_hovered = ctx.push, ctx.register_hit, ctx.is_hovered
    local label = state.skip_pill_label or "Skip ›"
    local is_start = (mode == "start")

    local fs = is_start and 20 or 22
    local sh = is_start and 48 or 52
    local sw = math.max(is_start and 208 or 214, estimate_text_w(label, fs) + 84)
    local sx1 = ctx.right_anchor
    local sx0 = sx1 - sw
    local sy1 = ctx.show_hud and (H - 194) or (H - 64)
    local sy0 = sy1 - sh
    local hov = is_hovered(sx0, sy0, sx1, sy1)

    local bg, alpha, bord_w, bc, txt_c
    if is_start then
        bg = hov and C.pill_hover or C.black
        alpha = hov and "&H10&" or "&H36&"
        bord_w = hov and 1.8 or 1.3
        bc = hov and C.white or C.border_hi
        txt_c = hov and C.white or C.text
    else
        bg = hov and C.red or C.black
        alpha = "&H08&"
        bord_w = 2.0
        bc = hov and C.red_hover or C.white
        txt_c = C.white
    end

    push(ass_rect(sx0, sy0, sx1, sy1, bg, alpha, 10, bord_w, bc))
    push(ass_text(sx0 + 22, (sy0 + sy1) * 0.5, 4, fs, txt_c, label, true))
    push(ass_text(sx1 - 18, (sy0 + sy1) * 0.5, 6, 15, hov and C.white or C.muted, "[S]", false))

    register_hit("skip_" .. mode, sx0, sy0, sx1, sy1, function()
        if mode == "recap" then
            do_skip_recap(false)
        elseif mode == "intro" then
            do_skip_intro(false)
        else
            do_skip_start()
        end
    end)
end

local function render_up_next_card(ctx)
    if not state.up_next_visible then return end

    local H = ctx.H
    local push, register_hit, is_hovered = ctx.push, ctx.register_hit, ctx.is_hovered

    local cw = 408
    local ch = 162
    local cx1 = ctx.right_anchor
    local cx0 = cx1 - cw
    local cy1 = ctx.show_hud and (H - 194) or (H - 56)
    if state.skip_pill_mode ~= nil then
        cy1 = cy1 - 66
    end
    local cy0 = cy1 - ch

    -- Card container (register catcher BEFORE foreground buttons)
    push(ass_rect(cx0, cy0, cx1, cy1, C.black, "&H08&", 12, 1.5, C.border_hi))
    register_hit("up_next_card", cx0, cy0, cx1, cy1, function() end)

    -- Animated red/gold countdown progress bar
    local total = math.max(1, state.up_next_total or (opts.autoskip_credits and 5 or 10))
    local rem = clamp(state.up_next_remaining or total, 0, total)
    local bar_frac = opts.autoplay and (rem / total) or 1
    local bx0, bx1 = cx0 + 12, cx1 - 12
    local bw = bx1 - bx0
    push(ass_rect(bx0, cy0 + 2, bx1, cy0 + 6, C.track_bg, "&H00&", 2))
    if bar_frac > 0.005 then
        local fill_x1 = bx0 + bw * bar_frac
        push(ass_rect(bx0, cy0 + 2, fill_x1, cy0 + 6, C.red, "&H00&", 2))
        local gold_x0 = math.max(bx0, fill_x1 - math.min(44, bw * bar_frac))
        if fill_x1 - gold_x0 >= 3 then
            push(ass_rect(gold_x0, cy0 + 2, fill_x1, cy0 + 6, C.amber, "&H00&", 2))
        end
    end

    -- Header row: SKIP CREDITS · UP NEXT (or UP NEXT) + keyboard hint
    local is_skip_credits = (opts.credits >= 0 or opts.autoskip_credits)
    local header_txt = is_skip_credits and "SKIP CREDITS · UP NEXT" or "UP NEXT"
    push(ass_text(cx0 + 22, cy0 + 20, 7, 15, is_skip_credits and C.amber or C.red, header_txt, true))
    push(ass_text(cx1 - 22, cy0 + 21, 9, 14, C.dim, "[S / Enter]", false))

    -- Next episode title
    local nlabel = (opts.next_label ~= "") and opts.next_label or "Next Episode"
    push(ass_text(cx0 + 22, cy0 + 48, 7, 22, C.white, truncate_to_width(nlabel, 22, cw - 44), true))

    -- Primary ("Play Next (5s)") & Secondary ("Watch Credits") buttons
    local btn_y1 = cy1 - 18
    local btn_y0 = btn_y1 - 44
    local play_x0 = cx0 + 22
    local play_x1 = play_x0 + 186
    local dis_x0 = play_x1 + 12
    local dis_x1 = cx1 - 22

    local play_label = opts.autoplay
        and string.format("Play Next (%ds)", math.max(0, math.ceil(rem)))
        or "Play Next"
    local play_hov = is_hovered(play_x0, btn_y0, play_x1, btn_y1)
    push(ass_rect(play_x0, btn_y0, play_x1, btn_y1, play_hov and C.red_hover or C.red, "&H00&", 8))
    push(ass_text((play_x0 + play_x1) * 0.5, (btn_y0 + btn_y1) * 0.5, 5, 18, C.white, play_label, true))
    register_hit("up_next_play", play_x0, btn_y0, play_x1, btn_y1, trigger_play_next)

    local dis_hov = is_hovered(dis_x0, btn_y0, dis_x1, btn_y1)
    push(ass_rect(dis_x0, btn_y0, dis_x1, btn_y1, dis_hov and C.pill_hover or C.pill, "&H00&", 8, 1.2, dis_hov and C.border_hi or C.border))
    push(ass_text((dis_x0 + dis_x1) * 0.5, (btn_y0 + btn_y1) * 0.5, 5, 17, C.text, "Watch Credits", false))
    register_hit("up_next_dismiss", dis_x0, btn_y0, dis_x1, btn_y1, dismiss_up_next)
end

local function render_credits_exit_pill(ctx)
    if not state.credits_exit_visible then return end

    local H = ctx.H
    local push, register_hit, is_hovered = ctx.push, ctx.register_hit, ctx.is_hovered

    local pw = 376
    local ph = 54
    local px1 = ctx.right_anchor
    local px0 = px1 - pw
    local py1 = ctx.show_hud and (H - 194) or (H - 56)
    local py0 = py1 - ph
    local mid_y = (py0 + py1) * 0.5

    push(ass_rect(px0, py0, px1, py1, C.black, "&H0A&", 10, 1.4, C.border_hi))
    push(ass_rect(px0 + 8, py0 + 11, px0 + 12, py1 - 11, C.amber, "&H00&", 2))
    register_hit("credits_exit_card", px0, py0, px1, py1, function() end)

    push(ass_text(px0 + 22, mid_y, 4, 18, C.text, "Credits · Press Q to Exit", true))

    -- Dismiss (×) button on far right
    local x_w = 28
    local x_x1 = px1 - 10
    local x_x0 = x_x1 - x_w
    local by0, by1 = py0 + 10, py1 - 10
    local x_hov = is_hovered(x_x0, by0, x_x1, by1)
    if x_hov then
        push(ass_rect(x_x0, by0, x_x1, by1, C.pill_hover, "&H00&", 6))
    end
    push(ass_text((x_x0 + x_x1) * 0.5, mid_y, 5, 18, x_hov and C.white or C.muted, "×", true))
    register_hit("credits_exit_dismiss", x_x0, by0, x_x1, by1, dismiss_credits_exit)

    -- Clickable "Close" button (exits mpv cleanly)
    local cl_w = 72
    local cl_x1 = x_x0 - 6
    local cl_x0 = cl_x1 - cl_w
    local cl_hov = is_hovered(cl_x0, by0, cl_x1, by1)
    push(ass_rect(cl_x0, by0, cl_x1, by1, cl_hov and C.red or C.pill, "&H00&", 7, 1.2, cl_hov and C.red_hover or C.border_hi))
    push(ass_text((cl_x0 + cl_x1) * 0.5, mid_y, 5, 16, C.white, "Close", true))
    register_hit("credits_exit_close", cl_x0, by0, cl_x1, by1, function()
        mp.command("quit")
    end)
end

local function render_episodes_drawer(ctx)
    if not state.ep_drawer_open then
        state.ep_drawer_box = nil
        return
    end

    local W, H = ctx.W, ctx.H
    local push, register_hit, is_hovered = ctx.push, ctx.register_hit, ctx.is_hovered

    local dw = ctx.drawer_w
    local dx1 = W
    local dx0 = W - dw
    local dy0 = 0
    local dy1 = ctx.show_hud and (H - 166) or H
    state.ep_drawer_box = { x0 = dx0, y0 = dy0, x1 = dx1, y1 = dy1 }

    -- Pure-black OLED #050507 at 94% opacity (&H0F& alpha) + thin left border
    push(ass_rect(dx0, dy0, dx1, dy1, C.drawer_bg, "&H0F&"))
    push(ass_rect(dx0, dy0, dx0 + 1, dy1, C.border, "&H38&"))
    register_hit("ep_drawer_bg", dx0, dy0, dx1, dy1, function() end)

    -- Header: "Episodes (N)" + clickable X close button
    local hdr_h = 64
    push(ass_rect(dx0, dy0 + hdr_h - 1, dx1, dy0 + hdr_h, C.border, "&H55&"))
    push(ass_text(dx0 + 22, dy0 + hdr_h * 0.5, 4, 23, C.white, string.format("Episodes (%d)", #state.episodes), true))

    local cl_s = 36
    local cl_x1 = dx1 - 18
    local cl_x0 = cl_x1 - cl_s
    local cl_y0 = dy0 + (hdr_h - cl_s) * 0.5
    local cl_y1 = cl_y0 + cl_s
    local cl_hov = is_hovered(cl_x0, cl_y0, cl_x1, cl_y1)
    push(ass_rect(cl_x0, cl_y0, cl_x1, cl_y1, cl_hov and C.red or C.pill, "&H00&", 8, 1.2, cl_hov and C.red_hover or C.border))
    push(ass_text((cl_x0 + cl_x1) * 0.5, (cl_y0 + cl_y1) * 0.5, 5, 20, C.white, "×", true))
    register_hit("ep_drawer_close", cl_x0, cl_y0, cl_x1, cl_y1, close_ep_drawer)

    -- Scrollable episode rows
    local list_y0 = dy0 + hdr_h + 10
    local list_y1 = dy1 - 12
    local row_h = 64
    local row_gap = 8
    local stride = row_h + row_gap
    local list_h = math.max(stride, list_y1 - list_y0)
    local visible_rows = math.max(1, math.floor((list_h + row_gap) / stride))
    local total_eps = #state.episodes
    local max_scroll = math.max(0, total_eps - visible_rows)
    state.ep_max_scroll = max_scroll
    state.ep_scroll = clamp(state.ep_scroll or 0, 0, max_scroll)

    local rx0 = dx0 + 16
    local rx1 = dx1 - (max_scroll > 0 and 20 or 16)

    for slot = 0, visible_rows - 1 do
        local idx = state.ep_scroll + slot + 1
        if idx > total_eps then break end

        local ep = state.episodes[idx]
        local ry0 = list_y0 + slot * stride
        local ry1 = ry0 + row_h
        local hov = is_hovered(rx0, ry0, rx1, ry1)

        local fill, bord_c, bord_w
        if ep.current then
            fill, bord_c, bord_w = C.red_dim, C.red, 1.4
        elseif hov then
            fill, bord_c, bord_w = C.pill_hover, C.border_hi, 1.3
        else
            fill, bord_c, bord_w = C.surface, C.border, 1.0
        end

        push(ass_rect(rx0, ry0, rx1, ry1, fill, "&H06&", 9, bord_w, bord_c))

        local has_prog = (ep.progress and ep.progress > 0.02)
        local mid_y = has_prog and (ry0 + 27) or (ry0 + 32)
        local badge_rx = rx1 - 12

        -- Current-playing red NOW badge
        if ep.current then
            local bw, bh = 48, 22
            local bx1 = badge_rx
            local bx0 = bx1 - bw
            push(ass_rect(bx0, mid_y - bh * 0.5, bx1, mid_y + bh * 0.5, C.red, "&H00&", 5))
            push(ass_text((bx0 + bx1) * 0.5, mid_y, 5, 13, C.white, "NOW", true))
            badge_rx = bx0 - 8
        end

        -- Amber FILLER tag
        if ep.filler == "filler" then
            local bw, bh = 60, 22
            local bx1 = badge_rx
            local bx0 = bx1 - bw
            push(ass_rect(bx0, mid_y - bh * 0.5, bx1, mid_y + bh * 0.5, C.amber_dim, "&H00&", 5, 1.0, C.amber))
            push(ass_text((bx0 + bx1) * 0.5, mid_y, 5, 12, C.amber, "FILLER", true))
            badge_rx = bx0 - 8
        end

        -- Episode code + title ("S2:E1 · Title")
        local ep_code = string.format("S%d:E%d", ep.season, ep.n)
        local row_label = (ep.title ~= "") and (ep_code .. " · " .. ep.title) or ep_code
        local text_avail = math.max(80, badge_rx - (rx0 + 14))
        local txt_c = (ep.current or hov) and C.white or C.text
        push(ass_text(rx0 + 14, mid_y, 4, 18, txt_c, truncate_to_width(row_label, 18, text_avail), ep.current or hov))

        -- Subtle bottom progress bar when progress > 0.02
        if has_prog then
            local pb_x0 = rx0 + 12
            local pb_x1 = rx1 - 12
            local pb_y0 = ry1 - 8
            local pb_y1 = ry1 - 4
            push(ass_rect(pb_x0, pb_y0, pb_x1, pb_y1, C.track_bg, "&H00&", 2))
            local pf = clamp(ep.progress, 0, 1)
            local pb_fx = pb_x0 + (pb_x1 - pb_x0) * pf
            if pb_fx > pb_x0 + 2 then
                push(ass_rect(pb_x0, pb_y0, pb_fx, pb_y1, ep.current and C.white or C.red, "&H00&", 2))
            end
        end

        local ep_ref = ep
        local ep_lbl = ep_code
        if not ep_ref.current then
            register_hit("ep_row_" .. idx, rx0, ry0, rx1, ry1, function()
                trigger_play_episode(ep_ref.id, ep_lbl)
            end)
        else
            register_hit("ep_row_" .. idx, rx0, ry0, rx1, ry1, close_ep_drawer)
        end
    end

    -- Subtle vertical scrollbar when list overflows
    if max_scroll > 0 then
        local sb_x0 = dx1 - 8
        local sb_x1 = dx1 - 4
        local sb_h = math.max(40, list_y1 - list_y0)
        push(ass_rect(sb_x0, list_y0, sb_x1, list_y1, C.track_bg, "&H55&", 2))
        local thumb_h = math.max(30, (visible_rows / total_eps) * sb_h)
        local thumb_y0 = list_y0 + (state.ep_scroll / max_scroll) * (sb_h - thumb_h)
        push(ass_rect(sb_x0, thumb_y0, sb_x1, thumb_y0 + thumb_h, C.border_hi, "&H18&", 2))
    end
end

local function render_toast(ctx)
    if not ctx.toast_active or not state.toast_text then return end
    local W = ctx.W
    local push = ctx.push
    local t_fs = 21
    local tw = math.max(180, estimate_text_w(state.toast_text, t_fs) + 52)
    local th = 46
    local center_x = state.ep_drawer_open and ((W - ctx.drawer_w) * 0.5) or (W * 0.5)
    local tx0 = center_x - tw * 0.5
    local tx1 = tx0 + tw
    local ty0 = ctx.show_hud and 118 or 34
    local ty1 = ty0 + th
    push(ass_rect(tx0, ty0, tx1, ty1, C.black, "&H0C&", 10, 1.4, C.border_hi))
    push(ass_rect(tx0 + 8, ty0 + 10, tx0 + 12, ty1 - 10, C.red, "&H00&", 2))
    push(ass_text((tx0 + tx1) * 0.5 + 4, (ty0 + ty1) * 0.5, 5, t_fs, C.white, state.toast_text, true))
end

--------------------------------------------------------------------------------
-- Render Pipeline
--------------------------------------------------------------------------------
render = function()
    update_canvas_size()
    update_binge_state()

    local now = mp.get_time()
    local toast_active = (state.toast_text ~= nil and now < state.toast_expire)
    if not toast_active then
        state.toast_text = nil
    end

    local show_hud = state.hud_visible or state.paused or state.dragging_seek
        or state.dragging_vol or state.ep_drawer_open
    local anything_visible = show_hud or (state.skip_pill_mode ~= nil)
        or state.up_next_visible or state.credits_exit_visible
        or state.ep_drawer_open or toast_active

    state.hitboxes = {}
    state.seek_box = nil
    state.vol_box = nil

    if not anything_visible then
        state.ep_drawer_box = nil
        if overlay_shown then
            overlay:remove()
            overlay_shown = false
            last_ass_data = nil
        end
        return
    end

    local W = state.res_x
    local H = state.res_y
    local pad = 56
    local drawer_w = math.min(440, math.floor(W * 0.42))
    local right_anchor = state.ep_drawer_open and (W - drawer_w - 24) or (W - pad)
    local mx, my = state.mouse_x, state.mouse_y
    local hover_ok = state.mouse_hover
    local lines = {}

    local ctx = {
        W = W,
        H = H,
        pad = pad,
        drawer_w = drawer_w,
        right_anchor = right_anchor,
        mx = mx,
        my = my,
        show_hud = show_hud,
        toast_active = toast_active,
        push = function(s)
            lines[#lines + 1] = s
        end,
        register_hit = function(id, x0, y0, x1, y1, action)
            state.hitboxes[#state.hitboxes + 1] = {
                id = id, x0 = x0, y0 = y0, x1 = x1, y1 = y1, action = action
            }
        end,
        is_hovered = function(x0, y0, x1, y1)
            return hover_ok and mx >= x0 and mx <= x1 and my >= y0 and my <= y1
        end,
    }

    if show_hud then
        render_top_bar(ctx)
        render_bottom_deck(ctx)
    end
    render_skip_pill(ctx)
    render_up_next_card(ctx)
    render_credits_exit_pill(ctx)
    render_episodes_drawer(ctx)
    render_toast(ctx)

    local ass_data = table.concat(lines, "\n")
    if ass_data ~= last_ass_data or overlay.res_x ~= W or overlay.res_y ~= H or not overlay_shown then
        overlay.res_x = W
        overlay.res_y = H
        overlay.data = ass_data
        overlay:update()
        overlay_shown = true
        last_ass_data = ass_data
    end
end

--------------------------------------------------------------------------------
-- Mouse & Click Hit-Testing
--------------------------------------------------------------------------------
local function seek_from_canvas_x(cx, exact)
    if not state.seek_box or not state.duration or state.duration <= 0 then return end
    local frac = clamp((cx - state.seek_box.x0) / math.max(1, state.seek_box.x1 - state.seek_box.x0), 0, 1)
    local target = frac * state.duration
    state.seek_drag_time = target
    if exact then
        mp.commandv("seek", string.format("%.3f", target), "absolute+exact")
    else
        mp.commandv("seek", string.format("%.3f", target), "absolute+keyframes")
    end
end

local function vol_from_canvas_x(cx)
    if not state.vol_box then return end
    local frac = clamp((cx - state.vol_box.x0) / math.max(1, state.vol_box.x1 - state.vol_box.x0), 0, 1)
    local vol = math.floor(frac * 100 + 0.5)
    mp.set_property_number("volume", vol)
    if state.mute and vol > 0 then
        mp.set_property_bool("mute", false)
    end
end

local function on_mouse_pos(_, pos)
    if not pos then return end
    update_canvas_size()
    local cx, cy = to_canvas_coords(pos.x or 0, pos.y or 0)
    local moved = (state.last_raw_mx ~= pos.x or state.last_raw_my ~= pos.y)
    state.last_raw_mx = pos.x
    state.last_raw_my = pos.y
    state.mouse_x = cx
    state.mouse_y = cy
    state.mouse_hover = (pos.hover == true)

    update_drawer_wheel_binding()

    if state.dragging_seek then
        seek_from_canvas_x(cx, false)
        render()
        return
    end

    if state.dragging_vol then
        vol_from_canvas_x(cx)
        render()
        return
    end

    if state.mouse_hover and moved then
        wake_hud()
    elseif not state.mouse_hover then
        render()
    end
end

local function on_mbtn_left(tbl)
    if not tbl then return end
    local ev = tbl.event

    if ev == "up" then
        if state.dragging_seek then
            seek_from_canvas_x(state.mouse_x, true)
            state.dragging_seek = false
            state.seek_drag_time = nil
            schedule_hide()
            render()
        end
        if state.dragging_vol then
            state.dragging_vol = false
            schedule_hide()
            render()
        end
        return
    end

    if ev ~= "down" and ev ~= "press" then return end
    local is_press = (ev == "press")

    local pos = mp.get_property_native("mouse-pos")
    if pos then
        update_canvas_size()
        state.mouse_x, state.mouse_y = to_canvas_coords(pos.x or 0, pos.y or 0)
        state.mouse_hover = (pos.hover == true)
    end
    local mx, my = state.mouse_x, state.mouse_y

    -- 1) Check interactive buttons/pills/drawer items in reverse z-order (topmost first)
    for i = #state.hitboxes, 1, -1 do
        local hb = state.hitboxes[i]
        if point_in_rect(mx, my, hb) then
            if click_pause_timer then
                click_pause_timer:kill()
                click_pause_timer = nil
            end
            hb.action()
            if state.hud_visible and not state.ep_drawer_open then schedule_hide() end
            return
        end
    end

    -- 2) Check seek bar & volume bar when HUD is visible
    local show_hud = state.hud_visible or state.paused or state.ep_drawer_open
    if show_hud then
        if state.seek_box and point_in_rect(mx, my, {
            x0 = state.seek_box.x0 - 10, y0 = state.seek_box.y0,
            x1 = state.seek_box.x1 + 10, y1 = state.seek_box.y1
        }) then
            if click_pause_timer then
                click_pause_timer:kill()
                click_pause_timer = nil
            end
            if is_press then
                seek_from_canvas_x(mx, true)
                state.seek_drag_time = nil
                schedule_hide()
            else
                state.dragging_seek = true
                seek_from_canvas_x(mx, false)
            end
            render()
            return
        end

        if state.vol_box and point_in_rect(mx, my, {
            x0 = state.vol_box.x0 - 6, y0 = state.vol_box.y0,
            x1 = state.vol_box.x1 + 6, y1 = state.vol_box.y1
        }) then
            if click_pause_timer then
                click_pause_timer:kill()
                click_pause_timer = nil
            end
            if is_press then
                vol_from_canvas_x(mx)
                schedule_hide()
            else
                state.dragging_vol = true
                vol_from_canvas_x(mx)
            end
            render()
            return
        end

        -- Clicking outside the Episodes drawer closes it cleanly (without pausing)
        if state.ep_drawer_open then
            close_ep_drawer()
            return
        end

        -- Clicking blank areas of top/bottom OLED deck keeps HUD alive without pausing
        if my <= state.top_bar_y1 or my >= state.bot_bar_y0 then
            wake_hud()
            return
        end
    end

    -- 3) Clicking main video canvas wakes HUD and toggles Play/Pause (debounced for double-click fullscreen)
    wake_hud()
    if click_pause_timer then
        click_pause_timer:kill()
        click_pause_timer = nil
    else
        click_pause_timer = mp.add_timeout(0.22, function()
            click_pause_timer = nil
            mp.commandv("cycle", "pause")
        end)
    end
end

--------------------------------------------------------------------------------
-- Property Observers, Lifecycle & Timers
--------------------------------------------------------------------------------
local function on_opts_changed()
    sync_parsed_opts()
    detect_chapters()
    load_episodes_file(false)
    render()
end

options.read_options(raw_opts, "tv_osd", on_opts_changed)
sync_parsed_opts()
state.shader_preset = opts.shader_preset
load_episodes_file(true)

mp.observe_property("mouse-pos", "native", on_mouse_pos)

mp.observe_property("pause", "bool", function(_, val)
    state.paused = (val == true)
    if state.paused then
        state.hud_visible = true
        if hide_timer then
            hide_timer:kill()
            hide_timer = nil
        end
    else
        schedule_hide()
    end
    render()
end)

mp.observe_property("time-pos", "number", function(_, val)
    if val ~= nil then
        state.time_pos = val
    end
end)

mp.observe_property("duration", "number", function(_, val)
    state.duration = val or 0
    if state.duration > 0 then
        detect_chapters()
    end
    render()
end)

mp.observe_property("chapter-list", "native", function()
    detect_chapters()
    if state.hud_visible or state.paused then render() end
end)

mp.observe_property("demuxer-cache-time", "number", function(_, val)
    state.cache_time = val or 0
end)

mp.observe_property("speed", "number", function(_, val)
    state.speed = (val and val > 0) and val or 1.0
end)

mp.observe_property("volume", "number", function(_, val)
    state.volume = val or 0
    if state.hud_visible or state.paused then render() end
end)

mp.observe_property("mute", "bool", function(_, val)
    state.mute = (val == true)
    if state.hud_visible or state.paused then render() end
end)

mp.observe_property("media-title", "string", function(_, val)
    state.media_title = val or ""
    if state.hud_visible or state.paused then render() end
end)

mp.observe_property("track-list", "native", function(_, val)
    state.tracks = val or {}
    if state.hud_visible or state.paused then render() end
end)

mp.observe_property("sub-visibility", "bool", function(_, val)
    state.sub_visibility = (val ~= false)
    if state.hud_visible or state.paused then render() end
end)

mp.observe_property("width", "number", function(_, val)
    state.vid_w = val or 0
end)

mp.observe_property("height", "number", function(_, val)
    state.vid_h = val or 0
end)

mp.observe_property("fullscreen", "bool", function()
    if click_pause_timer then
        click_pause_timer:kill()
        click_pause_timer = nil
    end
end)

mp.observe_property("osd-dimensions", "native", function()
    render()
end)

mp.register_event("file-loaded", function()
    -- Reset options from raw_opts before native chapter detection populates gaps
    sync_parsed_opts()

    -- Reset per-file playback state
    state.skip_pill_mode = nil
    state.skip_pill_label = nil
    state.skip_pill_target = nil
    state.recap_autoskipped = false
    state.recap_dismissed = false
    state.intro_autoskipped = false
    state.intro_dismissed = false
    state.skip_start_dismissed = false
    state.up_next_visible = false
    state.up_next_dismissed = false
    local cd = opts.autoskip_credits and 5 or 10
    state.up_next_total = cd
    state.up_next_remaining = cd
    state.up_next_last_tick = nil
    state.credits_exit_visible = false
    state.credits_exit_dismissed = false
    state.quitting_for_next = false
    set_up_next_enter_bound(false)

    cancel_replay_timer(false)
    stop_cropdetect()
    state.crop_mode = "off"

    -- Detect native MKV/MP4 chapters & ensure episodes list is loaded
    detect_chapters()
    if #state.episodes == 0 and opts.episodes_file ~= "" then
        load_episodes_file(true)
    end

    -- Apply initial Night Mode if requested and not already present
    local night_present = has_night_filter()
    if opts.night then
        if not night_present then
            set_night_mode(true, false)
        else
            state.night_active = true
        end
    else
        state.night_active = night_present
    end

    -- Apply initial Shader preset if requested and no shaders already loaded via CLI
    local existing_shaders = mp.get_property_native("glsl-shaders") or {}
    if opts.shader_preset ~= "off" and #existing_shaders == 0 then
        apply_shader_preset(opts.shader_preset, false)
    else
        state.shader_preset = opts.shader_preset
    end

    -- Show pure-black OLED HUD briefly on playback start (auto-hides after 2.5s)
    wake_hud()
end)

mp.register_event("shutdown", function()
    if hide_timer then hide_timer:kill() end
    if replay_timer then replay_timer:kill() end
    if crop_timer then crop_timer:kill() end
    if toast_timer then toast_timer:kill() end
    if click_pause_timer then click_pause_timer:kill() end
    set_up_next_enter_bound(false)
    set_drawer_esc_bound(false)
    set_drawer_wheel_bound(false)
    if overlay_shown then
        overlay:remove()
        overlay_shown = false
    end
end)

-- Periodic refresh tick (0.25s) for smooth progress, wall clock, skip pills & Up Next countdown
mp.add_periodic_timer(0.25, function()
    local was_skip = state.skip_pill_mode
    local was_up_next = state.up_next_visible
    local was_credits_exit = state.credits_exit_visible
    local was_cd = state.up_next_remaining
    update_binge_state()

    if state.hud_visible or state.paused or state.ep_drawer_open
        or state.skip_pill_mode ~= nil or state.up_next_visible or state.credits_exit_visible
        or was_skip ~= state.skip_pill_mode or was_up_next ~= state.up_next_visible
        or was_credits_exit ~= state.credits_exit_visible
        or was_cd ~= state.up_next_remaining or state.toast_text ~= nil then
        render()
    end
end)

--------------------------------------------------------------------------------
-- IPC Script Messages (Server / Phone Remote / Media Keys)
--------------------------------------------------------------------------------
mp.register_script_message("tv-next", function()
    if opts.has_next then trigger_play_next() end
end)

mp.register_script_message("tv-prev", function()
    if opts.has_prev then trigger_play_prev() end
end)

mp.register_script_message("tv-skip", function()
    handle_smart_skip()
end)

--------------------------------------------------------------------------------
-- Key Bindings
--------------------------------------------------------------------------------
mp.add_forced_key_binding("MBTN_LEFT", "tv_osd_mbtn_left", on_mbtn_left, { complex = true })

mp.add_forced_key_binding("Ctrl+0", "tv_osd_shader_off", function() apply_shader_preset("off", true) end)
mp.add_forced_key_binding("Ctrl+1", "tv_osd_shader_film", function() apply_shader_preset("film", true) end)
mp.add_forced_key_binding("Ctrl+2", "tv_osd_shader_anime", function() apply_shader_preset("anime", true) end)
mp.add_forced_key_binding("Ctrl+3", "tv_osd_shader_anime_hq", function() apply_shader_preset("anime_hq", true) end)

mp.add_forced_key_binding("b", "tv_osd_crop_lower", toggle_smart_crop)
mp.add_forced_key_binding("B", "tv_osd_crop_upper", toggle_smart_crop)

mp.add_forced_key_binding("r", "tv_osd_replay_lower", trigger_replay)
mp.add_forced_key_binding("R", "tv_osd_replay_upper", trigger_replay)

mp.add_forced_key_binding("s", "tv_osd_skip_lower", handle_smart_skip)
mp.add_forced_key_binding("S", "tv_osd_skip_upper", handle_smart_skip)

mp.add_forced_key_binding("n", "tv_osd_next_lower", function()
    if opts.has_next then trigger_play_next() else show_toast("No next episode") end
end)
mp.add_forced_key_binding("N", "tv_osd_next_upper", function()
    if opts.has_next then trigger_play_next() else show_toast("No next episode") end
end)

mp.add_forced_key_binding("P", "tv_osd_prev_upper", function()
    if opts.has_prev then trigger_play_prev() else show_toast("No previous episode") end
end)

mp.add_forced_key_binding("e", "tv_osd_episodes_lower", toggle_ep_drawer)
mp.add_forced_key_binding("E", "tv_osd_episodes_upper", toggle_ep_drawer)

msg.info("tv_osd loaded (preset=" .. opts.shader_preset
    .. ", night=" .. tostring(opts.night)
    .. ", live=" .. tostring(opts.is_live)
    .. ", series=" .. tostring(opts.is_series)
    .. ", episodes=" .. tostring(#state.episodes) .. ")")
