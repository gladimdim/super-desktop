// A small VT/xterm screen emulator for terminal plugins: feed it what a
// program prints, read back a grid of cells. It covers what shells, editors
// and TUIs use day to day (cursor movement, erasing, scroll regions, insert
// and delete, the alternate screen, 16/256/true colors, bold/italic/
// underline/inverse, application cursor keys, bracketed paste, device
// reports). Every character takes one cell: wide CJK characters and emoji
// take one cell too.

const DEFAULT_ATTR = Object.freeze({ fg: -1, bg: -1, bold: false, faint: false, italic: false, underline: false, inverse: false });

function blankLine(cols, attr) {
    const line = new Array(cols);
    for (let i = 0; i < cols; i++) line[i] = { ch: " ", attr: attr };
    return line;
}

// The 256-color palette beyond the 16 theme colors, as #rrggbb.
function xterm256(n) {
    if (n < 16) return null;
    if (n < 232) {
        const i = n - 16;
        const steps = [0, 95, 135, 175, 215, 255];
        const r = steps[Math.floor(i / 36)], g = steps[Math.floor(i / 6) % 6], b = steps[i % 6];
        return "#" + [r, g, b].map(v => v.toString(16).padStart(2, "0")).join("");
    }
    const v = 8 + (n - 232) * 10;
    const h = v.toString(16).padStart(2, "0");
    return "#" + h + h + h;
}

export class Screen {
    constructor(cols, rows, options) {
        options = options || {};
        this.cols = Math.max(2, cols | 0);
        this.rows = Math.max(1, rows | 0);
        this.scrollbackLimit = options.scrollback === undefined ? 2000 : options.scrollback;
        // Called with text the terminal answers with (device reports).
        this.respond = options.respond || function () {};
        this.onTitle = options.onTitle || function () {};
        this.onBell = options.onBell || function () {};
        this.reset();
    }

    reset() {
        this.attr = DEFAULT_ATTR;
        this.main = this.makeLines(this.rows);
        this.alt = null;
        this.lines = this.main;
        this.scrollback = [];
        this.x = 0;
        this.y = 0;
        this.wrapPending = false;
        this.top = 0;
        this.bottom = this.rows - 1;
        this.saved = null;
        this.cursorVisible = true;
        this.appCursor = false;
        this.bracketedPaste = false;
        this.autowrap = true;
        this.originMode = false;
        this.insertMode = false;
        this.tabs = this.defaultTabs(this.cols);
        this.state = "ground";
        this.params = "";
        this.osc = "";
        this.dirty = new Set();
        this.allDirty = true;
    }

    makeLines(n) {
        const lines = [];
        for (let i = 0; i < n; i++) lines.push(blankLine(this.cols, DEFAULT_ATTR));
        return lines;
    }

    defaultTabs(cols) {
        const tabs = new Set();
        for (let i = 8; i < cols; i += 8) tabs.add(i);
        return tabs;
    }

    get alternate() { return this.lines !== this.main; }

    markAll() { this.allDirty = true; }
    mark(y) { this.dirty.add(y); }

    // The rows changed since the last call, or "all".
    takeDirty() {
        if (this.allDirty) {
            this.allDirty = false;
            this.dirty.clear();
            return "all";
        }
        const rows = Array.from(this.dirty);
        this.dirty.clear();
        return rows;
    }

    resize(cols, rows) {
        cols = Math.max(2, cols | 0);
        rows = Math.max(1, rows | 0);
        if (cols === this.cols && rows === this.rows) return;
        const fit = (lines) => {
            for (const line of lines) {
                while (line.length < cols) line.push({ ch: " ", attr: DEFAULT_ATTR });
                line.length = cols;
            }
            return lines;
        };
        const reshape = (lines, isMain) => {
            fit(lines);
            while (lines.length > rows) {
                // Keep the cursor's line on screen: drop lines above it (into
                // the scrollback), else empty lines below it.
                if (this.lines === lines && this.y >= rows) {
                    const gone = lines.shift();
                    if (isMain) this.pushScrollback(gone);
                    this.y--;
                } else {
                    lines.pop();
                }
            }
            while (lines.length < rows) lines.push(blankLine(cols, DEFAULT_ATTR));
            return lines;
        };
        this.cols = cols;
        this.rows = rows;
        this.main = reshape(this.main, true);
        if (this.alt) this.alt = reshape(this.alt, false);
        this.lines = this.alt && this.lines !== this.main ? this.alt : this.main;
        fit(this.scrollback);
        this.top = 0;
        this.bottom = rows - 1;
        this.x = Math.min(this.x, cols - 1);
        this.y = Math.min(this.y, rows - 1);
        this.wrapPending = false;
        this.tabs = this.defaultTabs(cols);
        this.markAll();
    }

    pushScrollback(line) {
        if (this.scrollbackLimit <= 0) return;
        this.scrollback.push(line);
        if (this.scrollback.length > this.scrollbackLimit) this.scrollback.splice(0, this.scrollback.length - this.scrollbackLimit);
    }

    // ── output ──────────────────────────────────────────────────────────

    write(text) {
        for (const ch of text) this.consume(ch);
    }

    consume(ch) {
        const code = ch.codePointAt(0);
        switch (this.state) {
        case "ground":
            if (code === 0x1b) { this.state = "escape"; return; }
            if (code < 0x20 || code === 0x7f) { this.control(code); return; }
            this.print(ch);
            return;
        case "escape":
            this.escape(ch);
            return;
        case "csi":
            if (code === 0x1b) { this.state = "escape"; return; }
            if (code >= 0x40 && code <= 0x7e) {
                this.state = "ground";
                this.csi(ch, this.params);
                this.params = "";
            } else if (code < 0x20) {
                this.control(code);
            } else {
                this.params += ch;
                if (this.params.length > 64) { this.state = "ground"; this.params = ""; }
            }
            return;
        case "osc":
            if (code === 0x07) { this.oscEnd(); return; }
            if (code === 0x1b) { this.state = "oscEsc"; return; }
            if (this.osc.length < 4096) this.osc += ch;
            return;
        case "oscEsc":
            // ESC \ ends an OSC; anything else starts a new escape.
            if (ch === "\\") { this.oscEnd(); return; }
            this.oscEnd();
            this.state = "escape";
            this.escape(ch);
            return;
        case "charset":
            this.state = "ground";
            return;
        case "dcs":
            if (code === 0x1b) this.state = "dcsEsc";
            return;
        case "dcsEsc":
            this.state = ch === "\\" ? "ground" : "dcs";
            return;
        }
    }

    oscEnd() {
        this.state = "ground";
        const semi = this.osc.indexOf(";");
        const kind = semi < 0 ? this.osc : this.osc.slice(0, semi);
        if (kind === "0" || kind === "2") this.onTitle(this.osc.slice(semi + 1));
        this.osc = "";
    }

    control(code) {
        switch (code) {
        case 0x07: this.onBell(); break;
        case 0x08: if (this.x > 0) this.x--; this.wrapPending = false; break;
        case 0x09: this.tab(1); break;
        case 0x0a: case 0x0b: case 0x0c: this.lineFeed(); break;
        case 0x0d: this.x = 0; this.wrapPending = false; break;
        default: break;
        }
    }

    print(ch) {
        if (this.wrapPending) {
            this.wrapPending = false;
            if (this.autowrap) {
                this.x = 0;
                this.lineFeed();
            }
        }
        const line = this.lines[this.y];
        if (this.insertMode) {
            line.splice(this.x, 0, { ch: ch, attr: this.attr });
            line.length = this.cols;
        } else {
            line[this.x] = { ch: ch, attr: this.attr };
        }
        this.mark(this.y);
        if (this.x === this.cols - 1) this.wrapPending = true;
        else this.x++;
    }

    tab(count) {
        for (let i = 0; i < count; i++) {
            let next = this.cols - 1;
            for (let c = this.x + 1; c < this.cols; c++) if (this.tabs.has(c)) { next = c; break; }
            this.x = next;
        }
        this.wrapPending = false;
    }

    lineFeed() {
        this.wrapPending = false;
        if (this.y === this.bottom) this.scrollUp(1);
        else if (this.y < this.rows - 1) this.y++;
    }

    reverseIndex() {
        this.wrapPending = false;
        if (this.y === this.top) this.scrollDown(1);
        else if (this.y > 0) this.y--;
    }

    scrollUp(n) {
        for (let i = 0; i < n; i++) {
            const gone = this.lines.splice(this.top, 1)[0];
            if (this.top === 0 && !this.alternate) this.pushScrollback(gone);
            this.lines.splice(this.bottom, 0, blankLine(this.cols, this.blankAttr()));
        }
        this.markAll();
    }

    scrollDown(n) {
        for (let i = 0; i < n; i++) {
            this.lines.splice(this.bottom, 1);
            this.lines.splice(this.top, 0, blankLine(this.cols, this.blankAttr()));
        }
        this.markAll();
    }

    // Erased cells keep the current background, like xterm.
    blankAttr() {
        return this.attr.bg === -1 && !this.attr.inverse ? DEFAULT_ATTR
            : Object.freeze({ fg: -1, bg: this.attr.bg, bold: false, faint: false, italic: false, underline: false, inverse: this.attr.inverse });
    }

    escape(ch) {
        this.state = "ground";
        switch (ch) {
        case "[": this.state = "csi"; this.params = ""; break;
        case "]": this.state = "osc"; this.osc = ""; break;
        case "P": this.state = "dcs"; break;
        case "(": case ")": case "*": case "+": this.state = "charset"; break;
        case "7": this.saveCursor(); break;
        case "8": this.restoreCursor(); break;
        case "D": this.lineFeed(); break;
        case "E": this.x = 0; this.lineFeed(); break;
        case "M": this.reverseIndex(); break;
        case "H": this.tabs.add(this.x); break;
        case "c": this.reset(); break;
        default: break; // ESC = / ESC > (keypad modes) and others: nothing to draw.
        }
    }

    saveCursor() {
        this.saved = { x: this.x, y: this.y, attr: this.attr, wrapPending: this.wrapPending, originMode: this.originMode };
    }

    restoreCursor() {
        const s = this.saved || { x: 0, y: 0, attr: DEFAULT_ATTR, wrapPending: false, originMode: false };
        this.x = Math.min(s.x, this.cols - 1);
        this.y = Math.min(s.y, this.rows - 1);
        this.attr = s.attr;
        this.wrapPending = s.wrapPending;
        this.originMode = s.originMode;
    }

    moveTo(x, y) {
        const base = this.originMode ? this.top : 0;
        const limit = this.originMode ? this.bottom : this.rows - 1;
        this.x = Math.max(0, Math.min(this.cols - 1, x));
        this.y = Math.max(base, Math.min(limit, y + base));
        this.wrapPending = false;
    }

    eraseCells(y, from, to) {
        const line = this.lines[y];
        const attr = this.blankAttr();
        for (let c = Math.max(0, from); c < Math.min(this.cols, to); c++) line[c] = { ch: " ", attr: attr };
        this.mark(y);
    }

    csi(final, raw) {
        let prefix = "";
        if (raw.length && "?>=<".includes(raw[0])) { prefix = raw[0]; raw = raw.slice(1); }
        const intermediate = raw.match(/[ -\/]+$/);
        if (intermediate) raw = raw.slice(0, -intermediate[0].length);
        const p = raw.length ? raw.split(";").map(v => (v === "" ? NaN : parseInt(v.split(":")[0], 10))) : [];
        const arg = (i, d) => (Number.isNaN(p[i]) || p[i] === undefined || p[i] === 0 ? d : p[i]);
        if (prefix === "?") { if (final === "h" || final === "l") this.privateMode(p, final === "h"); return; }
        if (prefix === ">" || prefix === "=" || prefix === "<") {
            if (final === "c" && prefix === ">") this.respond("\x1b[>0;10;1c");
            return;
        }
        if (intermediate) return; // e.g. CSI SP q (cursor style)
        switch (final) {
        case "A": this.moveTo(this.x, this.y - (this.originMode ? this.top : 0) - arg(0, 1)); break;
        case "B": case "e": this.moveTo(this.x, this.y - (this.originMode ? this.top : 0) + arg(0, 1)); break;
        case "C": case "a": this.moveTo(this.x + arg(0, 1), this.y - (this.originMode ? this.top : 0)); break;
        case "D": this.moveTo(this.x - arg(0, 1), this.y - (this.originMode ? this.top : 0)); break;
        case "E": this.moveTo(0, this.y - (this.originMode ? this.top : 0) + arg(0, 1)); break;
        case "F": this.moveTo(0, this.y - (this.originMode ? this.top : 0) - arg(0, 1)); break;
        case "G": case "`": this.moveTo(arg(0, 1) - 1, this.y - (this.originMode ? this.top : 0)); break;
        case "d": this.moveTo(this.x, arg(0, 1) - 1); break;
        case "H": case "f": this.moveTo(arg(1, 1) - 1, arg(0, 1) - 1); break;
        case "J": this.eraseDisplay(Number.isNaN(p[0]) || p[0] === undefined ? 0 : p[0]); break;
        case "K": this.eraseLine(Number.isNaN(p[0]) || p[0] === undefined ? 0 : p[0]); break;
        case "X": this.eraseCells(this.y, this.x, this.x + arg(0, 1)); break;
        case "@": {
            const line = this.lines[this.y];
            const blanks = blankLine(Math.min(arg(0, 1), this.cols - this.x), this.blankAttr());
            line.splice(this.x, 0, ...blanks);
            line.length = this.cols;
            this.mark(this.y);
            break;
        }
        case "P": {
            const line = this.lines[this.y];
            const n = Math.min(arg(0, 1), this.cols - this.x);
            line.splice(this.x, n);
            while (line.length < this.cols) line.push({ ch: " ", attr: this.blankAttr() });
            this.mark(this.y);
            break;
        }
        case "L": case "M": {
            if (this.y < this.top || this.y > this.bottom) break;
            const savedTop = this.top;
            this.top = this.y;
            if (final === "L") this.scrollDown(Math.min(arg(0, 1), this.bottom - this.y + 1));
            else this.scrollUpInRegion(Math.min(arg(0, 1), this.bottom - this.y + 1));
            this.top = savedTop;
            this.x = 0;
            break;
        }
        case "S": this.scrollUpInRegion(arg(0, 1)); break;
        case "T": this.scrollDown(arg(0, 1)); break;
        case "r": {
            const top = arg(0, 1) - 1, bottom = arg(1, this.rows) - 1;
            if (top < bottom && bottom < this.rows) {
                this.top = top;
                this.bottom = bottom;
                this.moveTo(0, 0);
            }
            break;
        }
        case "m": this.sgr(p); break;
        case "h": case "l": if (p[0] === 4) this.insertMode = final === "h"; break;
        case "s": this.saveCursor(); break;
        case "u": this.restoreCursor(); break;
        case "n":
            if (p[0] === 6) this.respond("\x1b[" + (this.y + 1) + ";" + (this.x + 1) + "R");
            else if (p[0] === 5) this.respond("\x1b[0n");
            break;
        case "c": if (!p[0]) this.respond("\x1b[?1;2c"); break;
        case "g": if (!p[0]) this.tabs.delete(this.x); else if (p[0] === 3) this.tabs.clear(); break;
        case "I": this.tab(arg(0, 1)); break;
        case "Z": {
            for (let i = 0; i < arg(0, 1); i++) {
                let prev = 0;
                for (let c = this.x - 1; c > 0; c--) if (this.tabs.has(c)) { prev = c; break; }
                this.x = prev;
            }
            break;
        }
        default: break; // t (window ops), q, and the rest: nothing to draw.
        }
    }

    // Scrolls the region up without saving lines (inside a scroll region or
    // for deleted lines).
    scrollUpInRegion(n) {
        for (let i = 0; i < n; i++) {
            this.lines.splice(this.top, 1);
            this.lines.splice(this.bottom, 0, blankLine(this.cols, this.blankAttr()));
        }
        this.markAll();
    }

    eraseDisplay(mode) {
        if (mode === 0) {
            this.eraseCells(this.y, this.x, this.cols);
            for (let y = this.y + 1; y < this.rows; y++) this.eraseCells(y, 0, this.cols);
        } else if (mode === 1) {
            for (let y = 0; y < this.y; y++) this.eraseCells(y, 0, this.cols);
            this.eraseCells(this.y, 0, this.x + 1);
        } else if (mode === 2) {
            for (let y = 0; y < this.rows; y++) this.eraseCells(y, 0, this.cols);
        } else if (mode === 3) {
            this.scrollback = [];
            this.markAll();
        }
    }

    eraseLine(mode) {
        if (mode === 0) this.eraseCells(this.y, this.x, this.cols);
        else if (mode === 1) this.eraseCells(this.y, 0, this.x + 1);
        else if (mode === 2) this.eraseCells(this.y, 0, this.cols);
    }

    privateMode(params, on) {
        for (const mode of params) {
            switch (mode) {
            case 1: this.appCursor = on; break;
            case 6: this.originMode = on; this.moveTo(0, 0); break;
            case 7: this.autowrap = on; break;
            case 25: this.cursorVisible = on; this.markAll(); break;
            case 47: case 1047: this.setAlternate(on, false); break;
            case 1049: this.setAlternate(on, true); break;
            case 2004: this.bracketedPaste = on; break;
            default: break;
            }
        }
    }

    setAlternate(on, saveCursor) {
        if (on === this.alternate) return;
        if (on) {
            if (saveCursor) this.saveCursor();
            this.alt = this.makeLines(this.rows);
            this.lines = this.alt;
        } else {
            this.lines = this.main;
            this.alt = null;
            if (saveCursor) this.restoreCursor();
        }
        this.top = 0;
        this.bottom = this.rows - 1;
        this.markAll();
    }

    sgr(p) {
        if (p.length === 0) p = [0];
        let a = Object.assign({}, this.attr);
        for (let i = 0; i < p.length; i++) {
            const n = Number.isNaN(p[i]) ? 0 : p[i];
            if (n === 0) a = Object.assign({}, DEFAULT_ATTR);
            else if (n === 1) a.bold = true;
            else if (n === 2) a.faint = true;
            else if (n === 3) a.italic = true;
            else if (n === 4) a.underline = true;
            else if (n === 7) a.inverse = true;
            else if (n === 22) { a.bold = false; a.faint = false; }
            else if (n === 23) a.italic = false;
            else if (n === 24) a.underline = false;
            else if (n === 27) a.inverse = false;
            else if (n >= 30 && n <= 37) a.fg = n - 30;
            else if (n === 39) a.fg = -1;
            else if (n >= 40 && n <= 47) a.bg = n - 40;
            else if (n === 49) a.bg = -1;
            else if (n >= 90 && n <= 97) a.fg = n - 90 + 8;
            else if (n >= 100 && n <= 107) a.bg = n - 100 + 8;
            else if (n === 38 || n === 48) {
                let color = null;
                if (p[i + 1] === 5 && p[i + 2] !== undefined) {
                    const index = p[i + 2];
                    color = index < 16 ? index : xterm256(index);
                    i += 2;
                } else if (p[i + 1] === 2 && p[i + 4] !== undefined) {
                    color = "#" + [p[i + 2], p[i + 3], p[i + 4]].map(v => Math.max(0, Math.min(255, v | 0)).toString(16).padStart(2, "0")).join("");
                    i += 4;
                }
                if (color !== null) {
                    if (n === 38) a.fg = color; else a.bg = color;
                }
            }
        }
        this.attr = Object.freeze(a);
    }

    // ── reading ─────────────────────────────────────────────────────────

    // The visible text of a row (scrollback lines when `offset` > 0).
    lineText(row, offset) {
        const line = this.visibleLine(row, offset || 0);
        return line ? line.map(c => c.ch).join("") : "";
    }

    visibleLine(row, offset) {
        offset = Math.min(offset || 0, this.alternate ? 0 : this.scrollback.length);
        const index = row - offset;
        if (index >= 0) return this.lines[index];
        return this.scrollback[this.scrollback.length + index];
    }

    cell(row, col) {
        const line = this.lines[row];
        return line ? line[col] : null;
    }

    // Text the user typed, encoded for the program (keys, not pastes).
    static keyBytes(key, text, modifiers, appCursor) {
        const ctrl = (modifiers & 0x04000000) !== 0;
        const alt = (modifiers & 0x08000000) !== 0;
        const shift = (modifiers & 0x02000000) !== 0;
        const cursor = (letter) => (appCursor && !ctrl && !alt && !shift ? "\x1bO" : "\x1b[") + letter;
        const mod = 1 + (shift ? 1 : 0) + (alt ? 2 : 0) + (ctrl ? 4 : 0);
        const withMod = (letter) => (mod > 1 ? "\x1b[1;" + mod + letter : cursor(letter));
        const tilde = (n) => (mod > 1 ? "\x1b[" + n + ";" + mod + "~" : "\x1b[" + n + "~");
        switch (key) {
        case 0x01000004: case 0x01000005: return alt ? "\x1b\r" : "\r";     // Return, Enter
        case 0x01000003: return alt ? "\x1b\x7f" : (ctrl ? "\x08" : "\x7f"); // Backspace
        case 0x01000001: return "\t";                                         // Tab
        case 0x01000002: return "\x1b[Z";                                     // Backtab
        case 0x01000000: return "\x1b";                                       // Escape
        case 0x01000013: return withMod("A");
        case 0x01000015: return withMod("B");
        case 0x01000014: return withMod("C");
        case 0x01000012: return withMod("D");
        case 0x01000010: return withMod("H");                                 // Home
        case 0x01000011: return withMod("F");                                 // End
        case 0x01000006: return tilde(2);                                     // Insert
        case 0x01000007: return tilde(3);                                     // Delete
        case 0x01000016: return tilde(5);                                     // PageUp
        case 0x01000017: return tilde(6);                                     // PageDown
        default: break;
        }
        if (key >= 0x01000030 && key <= 0x0100003b) {                         // F1..F12
            const f = key - 0x01000030;
            if (f < 4) return "\x1bO" + "PQRS"[f];
            return tilde([15, 17, 18, 19, 20, 21, 23, 24][f - 4]);
        }
        if (ctrl && !alt) {
            if (key >= 0x41 && key <= 0x5a) return String.fromCharCode(key - 0x40); // Ctrl+A..Z
            if (key === 0x20) return "\x00";
            if (key === 0x5b) return "\x1b";
            if (key === 0x5c) return "\x1c";
            if (key === 0x5d) return "\x1d";
        }
        if (!text) return "";
        return alt ? "\x1b" + text : text;
    }
}

// The 16 ANSI colors from a SUPER DESKTOP theme (`sd.theme`).
export function themePalette(theme) {
    const t = theme || {};
    const pick = (k, d) => t[k] || d;
    return [
        pick("darkBackground", "#000000"), pick("red", "#cd3131"), pick("green", "#0dbc79"), pick("yellow", "#e5e510"),
        pick("blue", "#2472c8"), pick("magenta", "#bc3fbc"), pick("cyan", "#11a8cd"), pick("foreground", "#e5e5e5"),
        pick("muted", "#666666"), pick("red", "#f14c4c"), pick("green", "#23d18b"), pick("yellow", "#f5f543"),
        pick("blue", "#3b8eea"), pick("magenta", "#d670d6"), pick("cyan", "#29b8db"), pick("foreground", "#ffffff"),
    ];
}

function escapeHtml(s) {
    return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/ /g, "&nbsp;");
}

// One row as rich text: runs of cells with the same look become one span.
export function lineHtml(line, palette, fg, bg) {
    const color = (c, fallback) => (c === -1 ? fallback : (typeof c === "number" ? palette[c] : c));
    let html = "";
    let runAttr = null;
    let run = "";
    const flush = () => {
        if (!run) return;
        const a = runAttr;
        let f = color(a.fg === -1 || typeof a.fg !== "number" || !a.bold || a.fg >= 8 ? a.fg : a.fg + 8, fg);
        let b = color(a.bg, null);
        if (a.inverse) { const t = f; f = b || bg; b = t; }
        let style = "color:" + f + ";";
        if (b) style += "background-color:" + b + ";";
        if (a.bold) style += "font-weight:bold;";
        if (a.italic) style += "font-style:italic;";
        if (a.underline) style += "text-decoration:underline;";
        if (a.faint) style += "opacity:0.6;";
        html += "<span style=\"" + style + "\">" + escapeHtml(run) + "</span>";
        run = "";
    };
    for (const cell of line) {
        if (cell.attr !== runAttr) {
            flush();
            runAttr = cell.attr;
        }
        run += cell.ch;
    }
    flush();
    return html;
}
