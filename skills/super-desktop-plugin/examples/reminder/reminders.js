.pragma library

// Reminders are kept in sd.values.reminders as
// [{id, at (ms since epoch), text, fired (bool)}].

// A copy of the saved list (and of each item): change the copy, then
// sd.setValue it.
function list(values) {
    var r = values && values.reminders
    if (!r || typeof r.length !== "number") return []
    var out = []
    for (var i = 0; i < r.length; i++) out.push(Object.assign({}, r[i]))
    return out
}

function sorted(values) {
    return list(values).sort(function(a, b) { return a.at - b.at })
}

function pad(n) { return String(n).padStart(2, "0") }

function dateText(d) { return d.getFullYear() + "-" + pad(d.getMonth() + 1) + "-" + pad(d.getDate()) }
function timeText(d) { return pad(d.getHours()) + ":" + pad(d.getMinutes()) }

// "Today 14:30", "Tomorrow 09:00", "Mon 20 Oct 09:00", "2027-01-05 09:00".
function whenText(at) {
    var d = new Date(at)
    var now = new Date()
    var today = new Date(now.getFullYear(), now.getMonth(), now.getDate())
    var day = new Date(d.getFullYear(), d.getMonth(), d.getDate())
    var diff = Math.round((day - today) / 86400000)
    if (diff === 0) return "Today " + timeText(d)
    if (diff === 1) return "Tomorrow " + timeText(d)
    if (diff === -1) return "Yesterday " + timeText(d)
    if (diff > 1 && diff < 7)
        return ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][d.getDay()] + " " + timeText(d)
    return dateText(d) + " " + timeText(d)
}

// "in 5 min", "in 2 h 10 min", "in 3 days".
function relativeText(at) {
    // Rounded up, like a countdown: 9 min 20 s left reads "in 10 min".
    var mins = Math.ceil((at - Date.now()) / 60000)
    if (mins <= 0) return "now"
    if (mins < 60) return "in " + mins + " min"
    if (mins < 24 * 60) {
        var h = Math.floor(mins / 60), m = mins % 60
        return "in " + h + " h" + (m ? " " + m + " min" : "")
    }
    var days = Math.round(mins / (24 * 60))
    return "in " + days + (days === 1 ? " day" : " days")
}

// Parses "YYYY-MM-DD" and "HH:MM"; returns ms since epoch or NaN.
function parse(dateStr, timeStr) {
    var dm = /^\s*(\d{4})-(\d{1,2})-(\d{1,2})\s*$/.exec(dateStr)
    var tm = /^\s*(\d{1,2}):(\d{2})\s*$/.exec(timeStr)
    if (!dm || !tm) return NaN
    var y = +dm[1], mo = +dm[2], d = +dm[3], h = +tm[1], mi = +tm[2]
    if (mo < 1 || mo > 12 || d < 1 || d > 31 || h > 23 || mi > 59) return NaN
    var date = new Date(y, mo - 1, d, h, mi, 0, 0)
    if (date.getMonth() !== mo - 1) return NaN   // e.g. 31 Feb
    return date.getTime()
}

function newId() {
    return Date.now().toString(36) + Math.random().toString(36).slice(2, 7)
}

// ── Colors that stay readable in every Omarchy theme ──────────────────────
// A theme's `muted` is meant for borders: on its own background it can be
// nearly invisible, and some themes map red/yellow to shades of blue. So
// text colors are checked against the background (WCAG contrast).

function rgb(color) {
    var s = String(color)
    if (s.length === 9) s = "#" + s.slice(3)   // #aarrggbb
    var m = /^#?([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(s)
    return m ? [parseInt(m[1], 16), parseInt(m[2], 16), parseInt(m[3], 16)] : [128, 128, 128]
}

function luminance(color) {
    var c = rgb(color).map(function(v) {
        v /= 255
        return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4)
    })
    return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

function contrast(a, b) {
    var la = luminance(a), lb = luminance(b)
    return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05)
}

// `a` moved toward `b` by `t` (0..1), as "#rrggbb".
function mix(a, b, t) {
    var x = rgb(a), y = rgb(b)
    return "#" + [0, 1, 2].map(function(i) {
        return Math.round(x[i] + (y[i] - x[i]) * t).toString(16).padStart(2, "0")
    }).join("")
}

// Secondary text: the foreground faded toward the background by `t`, but
// never below a 4.5:1 contrast.
function soft(theme, t) {
    for (var f = t; f > 0; f -= 0.05) {
        var c = mix(theme.foreground, theme.background, f)
        if (contrast(c, theme.background) >= 4.5) return c
    }
    return theme.foreground
}

// The first of the theme's colors `names` that reads on its background
// (contrast >= `min`), else the foreground.
function readable(theme, names, min) {
    for (var i = 0; i < names.length; i++) {
        var c = theme[names[i]]
        if (c && contrast(c, theme.background) >= min) return c
    }
    return theme.foreground
}
