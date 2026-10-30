// sd-qml-host: draws one plugin QML surface off screen for SUPER DESKTOP.
//
// SUPER DESKTOP is a GTK layer-shell overlay, so plugin QML never gets a
// window of its own. This process loads the surface's QML file, renders it
// into shared memory with Qt Quick's software renderer and hands each frame
// to the overlay, which draws it in its slot and forwards pointer and key
// events back. A plugin that crashes or hangs ends only this process.
//
// Modes:
//   --serve --fd N --qml FILE --plugin-dir DIR --width W --height H
//       Talk to SUPER DESKTOP over the SOCK_SEQPACKET socket N (protocol in
//       src/plugins/host_protocol.rs).
//   --check FILE [--plugin-dir DIR]
//       Load the file, print {"ok":bool,"errors":[...],"warnings":[...]} on
//       stdout and exit 0 when it loads.
//   --snapshot FILE --out PNG --width W --height H [--scale S] [--theme JSON]
//       [--wait MS] [--do STEPS]
//       Render the surface once to a PNG, for agents to look at their work.
//       STEPS (`click "OK"; type hello; key Return; shot`) click, type and
//       check text first, as a user would; the report says what each did,
//       the calls the plugin made and the values it saved.

#include <QGuiApplication>
#include <QQmlComponent>
#include <QQmlContext>
#include <QQmlEngine>
#include <QQmlError>
#include <QQuickItem>
#include <QQuickRenderControl>
#include <QQuickRenderTarget>
#include <QQuickWindow>
#include <QSocketNotifier>
#include <QJsonArray>
#include <QJsonDocument>
#include <QJsonObject>
#include <QImage>
#include <QTimer>
#include <QProcess>
#include <QFileInfo>
#include <QDir>
#include <QUrl>
#include <QMouseEvent>
#include <QWheelEvent>
#include <QKeyEvent>
#include <QFocusEvent>
#include <QJSValue>
#include <QJSEngine>
#include <QCursor>
#include <QPalette>
#include <QPointer>
#include <QColor>
#include <QFile>
#include <QFileSystemWatcher>
#include <QSaveFile>
#include <QKeySequence>
#include <QRegularExpression>

#include <QProcessEnvironment>
#include <QStringDecoder>

#include <cmath>
#include <functional>
#include <csignal>
#include <pty.h>
#include <sys/ioctl.h>
#include <sys/wait.h>
#include <cstdio>
#include <cstring>
#include <sys/mman.h>
#include <sys/socket.h>
#include <unistd.h>
#include <fcntl.h>

static const int kMaxMessage = 1 << 20;

static int g_fd = -1;

// The environment the host was started with, before it switched Qt to the
// offscreen platform: programs a plugin starts get this one, so a Qt or GTK
// app opens on the user's desktop as usual.
static QProcessEnvironment &childEnvironment() {
    static QProcessEnvironment env = QProcessEnvironment::systemEnvironment();
    return env;
}

// A program on a pseudo-terminal, for plugins that are terminals: what it
// prints arrives as `output(text)`, `write(text)` types into it.
class Pty : public QObject {
    Q_OBJECT
    Q_PROPERTY(bool running READ running NOTIFY runningChanged)
    Q_PROPERTY(int pid READ pid CONSTANT)
public:
    ~Pty() override { terminate(SIGHUP); closeMaster(); }

    bool start(const QStringList &argv, const QString &cwd, int cols, int rows, const QVariantMap &env) {
        if (argv.isEmpty()) return false;
        struct winsize size;
        std::memset(&size, 0, sizeof size);
        size.ws_col = std::clamp(cols, 2, 1000);
        size.ws_row = std::clamp(rows, 1, 1000);
        QProcessEnvironment environment = childEnvironment();
        environment.insert("TERM", "xterm-256color");
        environment.insert("COLORTERM", "truecolor");
        for (auto it = env.begin(); it != env.end(); ++it) environment.insert(it.key(), it.value().toString());
        // Everything the child needs, prepared before fork.
        QList<QByteArray> args, vars;
        for (const auto &a : argv) args.append(a.toLocal8Bit());
        for (const auto &key : environment.keys()) vars.append((key + "=" + environment.value(key)).toLocal8Bit());
        std::vector<char *> cargs, cvars;
        for (auto &a : args) cargs.push_back(a.data());
        cargs.push_back(nullptr);
        for (auto &v : vars) cvars.push_back(v.data());
        cvars.push_back(nullptr);
        const QByteArray dir = cwd.toLocal8Bit();
        int master = -1;
        const pid_t pid = forkpty(&master, nullptr, nullptr, &size);
        if (pid < 0) return false;
        if (pid == 0) {
            if (!dir.isEmpty() && chdir(dir.constData()) != 0) _exit(127);
            ::signal(SIGPIPE, SIG_DFL);
            execvpe(cargs[0], cargs.data(), cvars.data());
            _exit(127);
        }
        ::fcntl(master, F_SETFD, FD_CLOEXEC);
        ::fcntl(master, F_SETFL, ::fcntl(master, F_GETFL) | O_NONBLOCK);
        m_master = master;
        m_pid = pid;
        m_notifier = new QSocketNotifier(master, QSocketNotifier::Read, this);
        connect(m_notifier, &QSocketNotifier::activated, this, &Pty::readable);
        return true;
    }

    bool running() const { return m_pid > 0; }
    int pid() const { return m_pid; }

    // Types `text` (UTF-8) into the program.
    Q_INVOKABLE bool write(const QString &text) {
        if (m_master < 0) return false;
        const QByteArray bytes = text.toUtf8();
        qsizetype done = 0;
        while (done < bytes.size()) {
            const ssize_t n = ::write(m_master, bytes.constData() + done, bytes.size() - done);
            if (n < 0) {
                if (errno == EINTR) continue;
                return false;
            }
            done += n;
        }
        return true;
    }

    Q_INVOKABLE void resize(int cols, int rows) {
        if (m_master < 0) return;
        struct winsize size;
        std::memset(&size, 0, sizeof size);
        size.ws_col = std::clamp(cols, 2, 1000);
        size.ws_row = std::clamp(rows, 1, 1000);
        ::ioctl(m_master, TIOCSWINSZ, &size);
    }

    // Sends a signal (default SIGTERM) to the program.
    Q_INVOKABLE void terminate(int signal = SIGTERM) {
        if (m_pid > 0) ::kill(m_pid, signal);
    }

signals:
    void output(const QString &text);
    void exited(int code);
    void runningChanged();

private slots:
    void readable() {
        char buffer[65536];
        for (;;) {
            const ssize_t n = ::read(m_master, buffer, sizeof buffer);
            if (n > 0) {
                const QString text = m_decoder.decode(QByteArrayView(buffer, n));
                if (!text.isEmpty()) emit output(text);
                continue;
            }
            if (n < 0 && errno == EINTR) continue;
            if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) return;
            finished();  // EOF or EIO: the program closed the terminal.
            return;
        }
    }

private:
    void finished() {
        closeMaster();
        int status = 0, code = -1;
        if (m_pid > 0) {
            // The slave side is gone; the child is exiting. Reap it.
            for (int i = 0; i < 200; ++i) {
                const pid_t r = ::waitpid(m_pid, &status, WNOHANG);
                if (r == m_pid) {
                    code = WIFEXITED(status) ? WEXITSTATUS(status) : 128 + WTERMSIG(status);
                    break;
                }
                if (r < 0) break;
                ::usleep(5000);
            }
        }
        m_pid = -1;
        emit runningChanged();
        emit exited(code);
    }

    void closeMaster() {
        if (m_notifier) {
            m_notifier->setEnabled(false);
            m_notifier->deleteLater();
            m_notifier = nullptr;
        }
        if (m_master >= 0) ::close(m_master);
        m_master = -1;
    }

    int m_master = -1;
    pid_t m_pid = -1;
    QSocketNotifier *m_notifier = nullptr;
    QStringDecoder m_decoder{QStringDecoder::Utf8};
};

static bool sendMessage(const QJsonObject &obj, int passFd = -1) {
    if (g_fd < 0) return false;
    QByteArray data = QJsonDocument(obj).toJson(QJsonDocument::Compact);
    if (data.size() > kMaxMessage) return false;
    struct iovec iov;
    iov.iov_base = data.data();
    iov.iov_len = data.size();
    struct msghdr msg;
    std::memset(&msg, 0, sizeof msg);
    msg.msg_iov = &iov;
    msg.msg_iovlen = 1;
    char control[CMSG_SPACE(sizeof(int))];
    if (passFd >= 0) {
        std::memset(control, 0, sizeof control);
        msg.msg_control = control;
        msg.msg_controllen = sizeof control;
        struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg);
        cmsg->cmsg_level = SOL_SOCKET;
        cmsg->cmsg_type = SCM_RIGHTS;
        cmsg->cmsg_len = CMSG_LEN(sizeof(int));
        std::memcpy(CMSG_DATA(cmsg), &passFd, sizeof(int));
    }
    ssize_t n;
    do {
        n = ::sendmsg(g_fd, &msg, MSG_NOSIGNAL);
    } while (n < 0 && errno == EINTR);
    return n == (ssize_t)data.size();
}

static QJsonObject errorJson(const QQmlError &e) {
    QJsonObject o;
    o["file"] = e.url().isLocalFile() ? e.url().toLocalFile() : e.url().toString();
    o["line"] = e.line();
    o["column"] = e.column();
    o["message"] = e.description();
    return o;
}

static QJsonArray errorsJson(const QList<QQmlError> &errors) {
    QJsonArray a;
    for (const auto &e : errors) a.append(errorJson(e));
    return a;
}

// The `sd` object every surface sees: the theme, logging, host calls.
class SdApi : public QObject {
    Q_OBJECT
    Q_PROPERTY(QVariantMap theme READ theme NOTIFY themeChanged)
    Q_PROPERTY(QString pluginDir READ pluginDir CONSTANT)
    Q_PROPERTY(QString pluginId READ pluginId CONSTANT)
    Q_PROPERTY(QString surfaceId READ surfaceId CONSTANT)
    Q_PROPERTY(QString instanceId READ instanceId CONSTANT)
    Q_PROPERTY(bool connected READ connected CONSTANT)
    Q_PROPERTY(int apiVersion READ apiVersion CONSTANT)
    // Real JS objects and arrays (a QVariantMap would hand QML Qt list
    // wrappers, for which Array.isArray is false).
    Q_PROPERTY(QJSValue values READ values NOTIFY valuesChanged)
    // Whether the overlay is on screen; visibleChanged(visible) follows it.
    Q_PROPERTY(bool visible READ visible NOTIFY visibleChanged)
public:
    SdApi(QJSEngine *js, QString pluginDir, QString pluginId, QString surfaceId, bool connected)
        : m_js(js), m_pluginDir(pluginDir), m_pluginId(pluginId), m_surfaceId(surfaceId),
          m_connected(connected), m_valuesJs(js->newObject()) {}

    QVariantMap theme() const { return m_theme; }
    QString pluginDir() const { return m_pluginDir; }
    QString pluginId() const { return m_pluginId; }
    QString surfaceId() const { return m_surfaceId; }
    QString instanceId() const { return m_instanceId; }
    void setInstanceId(const QString &id) { m_instanceId = id; }
    bool connected() const { return m_connected; }
    int apiVersion() const { return 1; }

    void setTheme(const QVariantMap &theme) {
        m_theme = theme;
        applyPalette(theme);
        emit themeChanged();
    }

    // Standard controls (QtQuick.Controls) draw with the application
    // palette: give it the theme's colors so they match without styling.
    static void applyPalette(const QVariantMap &theme) {
        auto color = [&](const char *key, const char *fallback) {
            QColor c(theme.value(key, theme.value(fallback)).toString());
            return c.isValid() ? c : QColor(theme.value(fallback).toString());
        };
        const QColor bg = color("background", "background");
        const QColor fg = color("foreground", "foreground");
        const QColor surface = color("surface", "background");
        const QColor dark = color("darkBackground", "background");
        const QColor muted = color("muted", "foreground");
        const QColor accent = color("accent", "foreground");
        const bool light = theme.value("mode").toString() == "light";
        // WCAG contrast, so text set on a color stays readable in every theme.
        auto luminance = [](const QColor &c) {
            auto channel = [](double v) { return v <= 0.03928 ? v / 12.92 : std::pow((v + 0.055) / 1.055, 2.4); };
            return 0.2126 * channel(c.redF()) + 0.7152 * channel(c.greenF()) + 0.0722 * channel(c.blueF());
        };
        auto contrast = [&](const QColor &a, const QColor &b) {
            const double la = luminance(a), lb = luminance(b);
            return (std::max(la, lb) + 0.05) / (std::min(la, lb) + 0.05);
        };
        auto mix = [](const QColor &a, const QColor &b, double t) {
            return QColor::fromRgbF(a.redF() + (b.redF() - a.redF()) * t, a.greenF() + (b.greenF() - a.greenF()) * t,
                                    a.blueF() + (b.blueF() - a.blueF()) * t);
        };
        // Text on the accent (highlighted and checked buttons): the theme's
        // own text or background color, whichever reads better.
        const QColor onAccent = contrast(fg, accent) >= contrast(bg, accent) ? fg : bg;
        // Placeholder text: the foreground faded toward the background, not
        // `muted` (meant for borders, nearly invisible in some themes).
        QColor placeholder = fg;
        for (double t = 0.45; t > 0; t -= 0.05) {
            const QColor c = mix(fg, dark, t);
            if (contrast(c, dark) >= 4.5) { placeholder = c; break; }
        }
        QPalette p;
        p.setColor(QPalette::Window, bg);
        p.setColor(QPalette::WindowText, fg);
        p.setColor(QPalette::Base, dark);
        p.setColor(QPalette::AlternateBase, surface);
        p.setColor(QPalette::Text, fg);
        p.setColor(QPalette::Button, surface);
        p.setColor(QPalette::ButtonText, fg);
        p.setColor(QPalette::BrightText, onAccent);
        p.setColor(QPalette::Highlight, accent);
        p.setColor(QPalette::HighlightedText, bg);
        p.setColor(QPalette::Light, light ? surface.darker(110) : surface.lighter(130));
        p.setColor(QPalette::Midlight, light ? surface.darker(105) : surface.lighter(115));
        p.setColor(QPalette::Mid, muted);
        // The Basic style fills highlighted/checked buttons, checked
        // switches, sliders and progress bars with Dark: the accent.
        p.setColor(QPalette::Dark, accent);
        p.setColor(QPalette::Shadow, QColor(0, 0, 0, 120));
        p.setColor(QPalette::PlaceholderText, placeholder);
        p.setColor(QPalette::ToolTipBase, surface);
        p.setColor(QPalette::ToolTipText, fg);
        p.setColor(QPalette::Link, accent);
        p.setColor(QPalette::Accent, accent);
        QGuiApplication::setPalette(p);
    }

    // The plugin's saved values: one JSON object in a file every surface of
    // the plugin shares. A change from any surface reaches all of them.
    void setDataFile(const QString &path) {
        m_dataFile = path;
        if (path.isEmpty()) return;
        QDir().mkpath(QFileInfo(path).absolutePath());
        m_watcher = new QFileSystemWatcher(this);
        // The directory too: a save replaces the file, which ends a file watch.
        m_watcher->addPath(QFileInfo(path).absolutePath());
        if (QFileInfo::exists(path)) m_watcher->addPath(path);
        auto reload = [this]() {
            if (QFileInfo::exists(m_dataFile) && !m_watcher->files().contains(m_dataFile))
                m_watcher->addPath(m_dataFile);
            loadValues(true);
        };
        connect(m_watcher, &QFileSystemWatcher::fileChanged, this, reload);
        connect(m_watcher, &QFileSystemWatcher::directoryChanged, this, reload);
        loadValues(false);
    }

    QJSValue values() const { return m_valuesJs; }

    Q_INVOKABLE QJSValue value(const QString &key, const QJSValue &fallback = QJSValue()) const {
        return m_values.contains(key) ? toJs(m_values.value(key)) : fallback;
    }

    bool visible() const { return m_visible; }
    void setVisible(bool visible) {
        if (visible == m_visible) return;
        m_visible = visible;
        emit visibleChanged(visible);
    }

    // Saves `value` under `key` (any JSON value; undefined removes the key).
    Q_INVOKABLE bool setValue(const QString &key, const QVariant &value) {
        if (m_dataFile.isEmpty()) return false;
        QVariantMap next = m_values;
        if (!value.isValid() || value.isNull()) next.remove(key);
        else next.insert(key, value.canConvert<QJSValue>() ? value.value<QJSValue>().toVariant() : value);
        const QByteArray bytes = QJsonDocument(QJsonObject::fromVariantMap(next)).toJson(QJsonDocument::Compact);
        if (bytes.size() > kMaxMessage) {
            log("sd.setValue: the plugin's values would exceed 1 MiB; not saved");
            return false;
        }
        QSaveFile file(m_dataFile);
        if (!file.open(QIODevice::WriteOnly) || file.write(bytes) != bytes.size() || !file.commit()) return false;
        setValues(next);
        emit valuesChanged();
        return true;
    }

    // Calls a SUPER DESKTOP method; `callback(result, error)` runs later.
    Q_INVOKABLE void call(const QString &method, const QVariant &params = QVariant(),
                          const QJSValue &callback = QJSValue()) {
        if (!m_connected) {
            // A snapshot does not reach SUPER DESKTOP: it says what was asked.
            m_calls.append(QJsonObject{{"method", method},
                                       {"params", params.isValid() ? QJsonValue::fromVariant(plain(params)) : QJsonValue()}});
            if (callback.isCallable()) {
                QJSValue cb = callback;
                QJSValue err = m_js->toScriptValue(QVariantMap{
                    {"code", "not_connected"},
                    {"message", "Not running inside SUPER DESKTOP (check or snapshot mode)."}});
                cb.call({QJSValue(QJSValue::NullValue), err});
            }
            return;
        }
        const int id = ++m_nextId;
        if (callback.isCallable()) m_pending.insert(id, callback);
        QJsonObject msg{{"t", "call"}, {"id", id}, {"method", method}};
        // plain(): Qt hands a JS object over as a QJSValue, which fromVariant drops.
        if (params.isValid()) msg["params"] = QJsonValue::fromVariant(plain(params));
        sendMessage(msg);
    }

    // Starts a program detached from this surface (argv list, no shell).
    Q_INVOKABLE bool run(const QStringList &argv) {
        if (argv.isEmpty()) return false;
        if (!m_connected) {
            // check and snapshot never start programs on the user's desktop.
            m_runs.append(QJsonArray::fromStringList(argv));
            return true;
        }
        QProcess process;
        process.setProgram(argv.first());
        process.setArguments(argv.mid(1));
        process.setWorkingDirectory(m_pluginDir);
        process.setProcessEnvironment(childEnvironment());
        return process.startDetached();
    }

    // A variable of the user's environment (as programs see it), or "".
    Q_INVOKABLE QString env(const QString &name) const {
        return childEnvironment().value(name);
    }

    // Starts `argv` on a pseudo-terminal; returns the Pty or null. Options:
    // {cols, rows, cwd, env: {NAME: value}}. At most 8 per surface.
    Q_INVOKABLE QObject *openPty(const QStringList &argv, const QVariantMap &options = QVariantMap()) {
        m_ptys.removeAll(nullptr);
        if (m_ptys.size() >= 8) {
            log("sd.openPty: at most 8 terminals per surface");
            return nullptr;
        }
        auto *pty = new Pty();
        const QString cwd = options.value("cwd", m_pluginDir).toString();
        if (!pty->start(argv, cwd, options.value("cols", 80).toInt(), options.value("rows", 24).toInt(),
                        options.value("env").toMap())) {
            delete pty;
            return nullptr;
        }
        // Owned here (it ends with the surface), not by the JS garbage collector.
        QQmlEngine::setObjectOwnership(pty, QQmlEngine::CppOwnership);
        pty->setParent(this);
        m_ptys.append(pty);
        return pty;
    }

    // Runs a program and gives `callback({code, stdout, stderr})` when it ends.
    Q_INVOKABLE void exec(const QStringList &argv, const QJSValue &callback = QJSValue()) {
        if (argv.isEmpty()) return;
        auto *p = new QProcess(this);
        p->setWorkingDirectory(m_pluginDir);
        QJSValue cb = callback;
        connect(p, &QProcess::finished, this, [this, p, cb](int code, QProcess::ExitStatus) mutable {
            if (cb.isCallable()) {
                QJSValue r = m_js->toScriptValue(QVariantMap{
                    {"code", code},
                    {"stdout", QString::fromUtf8(p->readAllStandardOutput())},
                    {"stderr", QString::fromUtf8(p->readAllStandardError())}});
                cb.call({r});
            }
            p->deleteLater();
        });
        connect(p, &QProcess::errorOccurred, this, [this, p, cb](QProcess::ProcessError) mutable {
            if (p->state() != QProcess::NotRunning) return;
            if (cb.isCallable()) {
                QJSValue r = m_js->toScriptValue(QVariantMap{
                    {"code", -1}, {"stdout", ""}, {"stderr", p->errorString()}});
                cb.call({r});
            }
            p->deleteLater();
        });
        p->setProcessEnvironment(childEnvironment());
        p->start(argv.first(), argv.mid(1));
    }

    Q_INVOKABLE void log(const QString &text) {
        if (m_connected) sendMessage(QJsonObject{{"t", "log"}, {"level", "info"}, {"message", text}});
        else m_logs.append(text);
    }

    // What a snapshot's plugin asked for, ran and logged, and its values now.
    const QJsonArray &calls() const { return m_calls; }
    const QJsonArray &runs() const { return m_runs; }
    const QStringList &logs() const { return m_logs; }
    QJsonObject savedValues() const { return QJsonObject::fromVariantMap(m_values); }

    // Something that happened in SUPER DESKTOP, for every running surface of
    // the plugin. Unknown names are ignored, so the overlay can grow.
    void deliver(const QJsonObject &m) {
        if (m["name"].toString() == "cardButton")
            emit cardButtonClicked(m["button"].toString(), m["card"].toObject().toVariantMap());
    }

    void reply(int id, const QJsonValue &result, const QJsonValue &error) {
        auto it = m_pending.find(id);
        if (it == m_pending.end()) return;
        QJSValue cb = it.value();
        m_pending.erase(it);
        cb.call({m_js->toScriptValue(result.toVariant()), m_js->toScriptValue(error.toVariant())});
    }

signals:
    void themeChanged();
    void valuesChanged();
    // Emitted when the overlay is shown or hidden.
    void visibleChanged(bool visible);
    // One of the plugin's cardButtons was clicked on a terminal card; `card`
    // is that card as cards.list describes it.
    void cardButtonClicked(const QString &button, const QVariant &card);

private:
    void loadValues(bool notify) {
        QFile file(m_dataFile);
        QVariantMap loaded;
        if (file.open(QIODevice::ReadOnly)) {
            const auto doc = QJsonDocument::fromJson(file.read(kMaxMessage));
            if (doc.isObject()) loaded = doc.object().toVariantMap();
        }
        if (loaded == m_values) return;
        setValues(loaded);
        if (notify) emit valuesChanged();
    }

    void setValues(const QVariantMap &values) {
        m_values = values;
        m_valuesJs = toJs(values);
    }

    static QVariant plain(const QVariant &v) {
        if (v.typeId() == qMetaTypeId<QJSValue>()) return plain(v.value<QJSValue>().toVariant());
        if (v.typeId() == QMetaType::QVariantMap) {
            QVariantMap map = v.toMap();
            for (auto it = map.begin(); it != map.end(); ++it) it.value() = plain(it.value());
            return map;
        }
        if (v.typeId() == QMetaType::QVariantList) {
            QVariantList list = v.toList();
            for (auto &item : list) item = plain(item);
            return list;
        }
        return v;
    }

    // JSON-shaped variants as plain JS values: objects, arrays, scalars.
    QJSValue toJs(const QVariant &v) const {
        switch (v.typeId()) {
        case QMetaType::QVariantMap: {
            QJSValue object = m_js->newObject();
            const QVariantMap map = v.toMap();
            for (auto it = map.cbegin(); it != map.cend(); ++it) object.setProperty(it.key(), toJs(it.value()));
            return object;
        }
        case QMetaType::QVariantList: {
            const QVariantList list = v.toList();
            QJSValue array = m_js->newArray(list.size());
            for (int i = 0; i < list.size(); ++i) array.setProperty(i, toJs(list.at(i)));
            return array;
        }
        default:
            return m_js->toScriptValue(v);
        }
    }

    QJSEngine *m_js;
    QString m_pluginDir, m_pluginId, m_surfaceId, m_instanceId;
    bool m_connected;
    QVariantMap m_theme;
    QVariantMap m_values;
    QJSValue m_valuesJs;
    bool m_visible = true;
    QString m_dataFile;
    QFileSystemWatcher *m_watcher = nullptr;
    QList<QPointer<Pty>> m_ptys;
    QJsonArray m_calls, m_runs;
    QStringList m_logs;
    int m_nextId = 0;
    QHash<int, QJSValue> m_pending;
};

static QVariantMap defaultTheme() {
    return QVariantMap{
        {"name", "default"}, {"mode", "dark"},
        {"accent", "#7aa2f7"}, {"background", "#1a1b26"}, {"foreground", "#c0caf5"},
        {"darkBackground", "#16161e"},
        {"muted", "#565f89"}, {"surface", "#24283b"}, {"selection", "#33467c"},
        {"red", "#f7768e"}, {"green", "#9ece6a"}, {"yellow", "#e0af68"},
        {"blue", "#7aa2f7"}, {"magenta", "#bb9af7"}, {"cyan", "#7dcfff"},
        {"orange", "#ff9e64"}, {"fontFamily", "monospace"}, {"fontSize", 13},
    };
}

static QVariantMap parseTheme(const QString &json) {
    QVariantMap theme = defaultTheme();
    if (json.isEmpty()) return theme;
    auto doc = QJsonDocument::fromJson(json.toUtf8());
    if (!doc.isObject()) return theme;
    const auto obj = doc.object();
    for (auto it = obj.begin(); it != obj.end(); ++it) theme.insert(it.key(), it.value().toVariant());
    return theme;
}

class Surface : public QObject {
    Q_OBJECT
public:
    Surface(const QString &qmlFile, const QString &pluginDir, const QString &pluginId,
            const QString &surfaceId, bool connected, const QVariantMap &theme)
        : m_qmlFile(qmlFile) {
        m_control = new QQuickRenderControl(this);
        m_window = new QQuickWindow(m_control);
        m_window->setColor(Qt::transparent);
        m_engine = new QQmlEngine(this);
        m_engine->setOutputWarningsToStandardError(false);
        connect(m_engine, &QQmlEngine::warnings, this, [this](const QList<QQmlError> &w) {
            for (const auto &e : w) {
                m_warnings.append(errorJson(e));
                if (g_fd >= 0)
                    sendMessage(QJsonObject{{"t", "log"}, {"level", "warning"}, {"error", errorJson(e)},
                                            {"message", e.toString()}});
            }
        });
        // Plugin-local QML modules and components resolve from the plugin folder.
        m_engine->addImportPath(pluginDir);
        // Components that ship with SUPER DESKTOP (import SuperDesktop.Terminal).
        m_engine->addImportPath("qrc:/qml");
        m_api = new SdApi(m_engine, pluginDir, pluginId, surfaceId, connected);
        m_api->setParent(this);
        m_api->setTheme(theme);
        m_engine->rootContext()->setContextProperty("sd", m_api);

        connect(m_control, &QQuickRenderControl::renderRequested, this, &Surface::scheduleRender);
        connect(m_control, &QQuickRenderControl::sceneChanged, this, &Surface::scheduleRender);
        m_renderTimer.setSingleShot(true);
        m_renderTimer.setInterval(0);
        connect(&m_renderTimer, &QTimer::timeout, this, &Surface::render);
    }

    ~Surface() override {
        delete m_root;
        delete m_window;
        if (m_map) munmap(m_map, m_mapSize);
        if (m_memfd >= 0) ::close(m_memfd);
    }

    SdApi *api() const { return m_api; }
    const QJsonArray &warnings() const { return m_warnings; }

    // Loads the QML file. Returns the errors, empty when it loaded.
    QJsonArray load() {
        QQmlComponent component(m_engine, QUrl::fromLocalFile(m_qmlFile));
        if (component.isError()) return errorsJson(component.errors());
        QObject *obj = component.create();
        if (!obj) return errorsJson(component.errors());
        if (qobject_cast<QWindow *>(obj)) {
            delete obj;
            QJsonObject e{{"file", m_qmlFile}, {"line", 1}, {"column", 1},
                          {"message", "The root of a surface must be an Item (Rectangle, Item, "
                                      "ColumnLayout...), not a Window: SUPER DESKTOP gives each "
                                      "surface its place."}};
            return QJsonArray{e};
        }
        m_root = qobject_cast<QQuickItem *>(obj);
        if (!m_root) {
            delete obj;
            QJsonObject e{{"file", m_qmlFile}, {"line", 1}, {"column", 1},
                          {"message", "The root object of a surface must be a QtQuick Item."}};
            return QJsonArray{e};
        }
        m_root->setParentItem(m_window->contentItem());
        m_root->setFocus(true);
        return {};
    }

    void resize(int w, int h) {
        w = std::max(1, w);
        h = std::max(1, h);
        m_width = w;
        m_height = h;
        m_window->setGeometry(0, 0, w, h);
        m_window->contentItem()->setSize(QSizeF(w, h));
        if (m_root) m_root->setSize(QSizeF(w, h));
        scheduleRender();
    }

    void setVisible(bool visible) {
        m_visible = visible;
        m_api->setVisible(visible);
        if (visible) scheduleRender();
    }

    void ack() {
        m_awaitingAck = false;
        if (m_dirty) scheduleRender();
    }

    // Renders one frame. grabWindow() drives the render control's software
    // renderer and returns the device-pixel image; a QQuickRenderTarget on a
    // paint device left the image empty with Qt 6.12's software backend.
    QImage renderFrame() {
        QImage image = m_window->grabWindow();
        if (image.format() != QImage::Format_ARGB32_Premultiplied)
            image = image.convertToFormat(QImage::Format_ARGB32_Premultiplied);
        // At a fractional scale whose size rounds down (83 px at 1.666 is
        // 138.33), the renderer leaves the last column or row unpainted:
        // it holds stale pixels (dots of an earlier glyph). Repeat its
        // neighbor instead.
        const qreal dpr = image.devicePixelRatio();
        const auto unpainted = [dpr](int logical) {
            const qreal exact = logical * dpr;
            return exact - std::floor(exact) > 1e-6 && exact - std::floor(exact) < 0.5;
        };
        if (image.width() >= 2 && unpainted(m_width)) {
            for (int y = 0; y < image.height(); ++y) {
                QRgb *line = reinterpret_cast<QRgb *>(image.scanLine(y));
                line[image.width() - 1] = line[image.width() - 2];
            }
        }
        if (image.height() >= 2 && unpainted(m_height))
            std::memcpy(image.scanLine(image.height() - 1), image.constScanLine(image.height() - 2), size_t(image.bytesPerLine()));
        return image;
    }

    QQuickWindow *window() const { return m_window; }
    QQuickItem *root() const { return m_root; }
    // Lays out what changed (layouts and anchors settle when polished, which
    // otherwise happens only when a frame is drawn).
    void polish() { m_control->polishItems(); }

public slots:
    void scheduleRender() {
        m_dirty = true;
        if (g_fd < 0 || !m_visible || m_awaitingAck) return;
        if (!m_renderTimer.isActive()) m_renderTimer.start();
    }

private slots:
    void render() {
        if (!m_root || m_awaitingAck || !m_visible) return;
        m_dirty = false;
        const QImage image = renderFrame();
        if (image.isNull()) {
            sendMessage(QJsonObject{{"t", "log"}, {"level", "error"},
                                    {"message", "Qt Quick rendered no image for this surface."}});
            return;
        }
        const int pw = image.width(), ph = image.height();
        if (!ensureBuffer(pw, ph, image.devicePixelRatio())) return;
        for (int y = 0; y < ph; ++y)
            std::memcpy(static_cast<uchar *>(m_map) + size_t(y) * pw * 4, image.constScanLine(y), size_t(pw) * 4);
        m_awaitingAck = true;
        sendMessage(QJsonObject{{"t", "frame"}, {"seq", ++m_seq}});
    }

private:
    bool ensureBuffer(int pw, int ph, qreal scale) {
        if (m_map && pw == m_bufW && ph == m_bufH) return true;
        if (m_map) munmap(m_map, m_mapSize);
        if (m_memfd >= 0) ::close(m_memfd);
        m_map = nullptr;
        m_memfd = memfd_create("sd-qml-surface", MFD_CLOEXEC);
        if (m_memfd < 0) return false;
        m_mapSize = size_t(pw) * size_t(ph) * 4;
        if (ftruncate(m_memfd, m_mapSize) != 0) return false;
        void *p = mmap(nullptr, m_mapSize, PROT_READ | PROT_WRITE, MAP_SHARED, m_memfd, 0);
        if (p == MAP_FAILED) return false;
        m_map = p;
        m_bufW = pw;
        m_bufH = ph;
        sendMessage(QJsonObject{{"t", "buffer"}, {"width", pw}, {"height", ph}, {"stride", pw * 4},
                                {"scale", scale}, {"format", "argb32-premultiplied"}},
                    m_memfd);
        return true;
    }

    QString m_qmlFile;
    QQuickRenderControl *m_control = nullptr;
    QQuickWindow *m_window = nullptr;
    QQmlEngine *m_engine = nullptr;
    QQuickItem *m_root = nullptr;
    SdApi *m_api = nullptr;
    QTimer m_renderTimer;
    QJsonArray m_warnings;
    bool m_visible = true;
    bool m_awaitingAck = false;
    bool m_dirty = true;
    int m_width = 1, m_height = 1;
    int m_seq = 0;
    int m_memfd = -1;
    void *m_map = nullptr;
    size_t m_mapSize = 0;
    int m_bufW = 0, m_bufH = 0;
};

// Reads messages from SUPER DESKTOP and turns them into Qt events.
class Connection : public QObject {
    Q_OBJECT
public:
    Connection(Surface *surface) : m_surface(surface) {
        m_notifier = new QSocketNotifier(g_fd, QSocketNotifier::Read, this);
        connect(m_notifier, &QSocketNotifier::activated, this, &Connection::readAll);
    }

private slots:
    void readAll() {
        QByteArray buf(kMaxMessage, Qt::Uninitialized);
        for (;;) {
            ssize_t n = ::recv(g_fd, buf.data(), buf.size(), MSG_DONTWAIT);
            if (n == 0) {
                QCoreApplication::exit(0);
                return;
            }
            if (n < 0) {
                if (errno == EINTR) continue;
                if (errno == EAGAIN || errno == EWOULDBLOCK) return;
                QCoreApplication::exit(0);
                return;
            }
            auto doc = QJsonDocument::fromJson(QByteArray(buf.constData(), n));
            if (doc.isObject()) handle(doc.object());
        }
    }

private:
    void handle(const QJsonObject &m) {
        const QString t = m["t"].toString();
        QQuickWindow *win = m_surface->window();
        if (t == "ack") {
            m_surface->ack();
        } else if (t == "resize") {
            m_surface->resize(m["width"].toInt(), m["height"].toInt());
        } else if (t == "visible") {
            m_surface->setVisible(m["value"].toBool());
        } else if (t == "theme") {
            m_surface->api()->setTheme(parseTheme(QString::fromUtf8(
                QJsonDocument(m["theme"].toObject()).toJson(QJsonDocument::Compact))));
        } else if (t == "reply") {
            m_surface->api()->reply(m["id"].toInt(), m["result"], m["error"]);
        } else if (t == "event") {
            m_surface->api()->deliver(m);
        } else if (t == "pointer") {
            pointer(win, m);
        } else if (t == "scroll") {
            const QPointF pos(m["x"].toDouble(), m["y"].toDouble());
            const QPoint angle(std::lround(-m["dx"].toDouble() * 120), std::lround(-m["dy"].toDouble() * 120));
            QWheelEvent ev(pos, pos, QPoint(), angle, m_buttons, modifiers(m), Qt::NoScrollPhase, false);
            ev.setAccepted(false);
            QCoreApplication::sendEvent(win, &ev);
            // Nothing in the QML used it (no Flickable, ListView or wheel
            // handler under the pointer): the overlay may scroll the page
            // around the surface instead.
            if (!ev.isAccepted())
                sendMessage(QJsonObject{{"t", "scrollIgnored"}, {"dx", m["dx"]}, {"dy", m["dy"]}});
        } else if (t == "key") {
            const bool press = m["kind"].toString() == "press";
            QKeyEvent ev(press ? QEvent::KeyPress : QEvent::KeyRelease, m["key"].toInt(), modifiers(m),
                         m["text"].toString(), m["repeat"].toBool());
            QCoreApplication::sendEvent(win, &ev);
        } else if (t == "focus") {
            QFocusEvent ev(m["value"].toBool() ? QEvent::FocusIn : QEvent::FocusOut, Qt::OtherFocusReason);
            QCoreApplication::sendEvent(win, &ev);
        } else if (t == "quit") {
            QCoreApplication::exit(0);
        }
    }

    static Qt::KeyboardModifiers modifiers(const QJsonObject &m) {
        return Qt::KeyboardModifiers(m["mods"].toInt());
    }

    void pointer(QQuickWindow *win, const QJsonObject &m) {
        const QString kind = m["kind"].toString();
        const QPointF pos(m["x"].toDouble(), m["y"].toDouble());
        const Qt::MouseButton button = Qt::MouseButton(m["button"].toInt());
        if (kind == "leave") {
            QEvent ev(QEvent::Leave);
            QCoreApplication::sendEvent(win, &ev);
            return;
        }
        if (kind == "enter") {
            QEnterEvent ev(pos, pos, pos);
            QCoreApplication::sendEvent(win, &ev);
            return;
        }
        QEvent::Type type = QEvent::MouseMove;
        if (kind == "press") {
            type = QEvent::MouseButtonPress;
            m_buttons |= button;
        } else if (kind == "release") {
            type = QEvent::MouseButtonRelease;
            m_buttons &= ~button;
        } else if (kind == "double") {
            type = QEvent::MouseButtonDblClick;
        }
        QMouseEvent ev(type, pos, pos, pos, kind == "move" ? Qt::NoButton : button, m_buttons,
                       modifiers(m));
        QCoreApplication::sendEvent(win, &ev);
        reportCursor(win);
    }

    // The overlay draws the pointer: tell it when QML asks for another shape.
    void reportCursor(QQuickWindow *win) {
        const int shape = int(win->cursor().shape());
        if (shape == m_cursor) return;
        m_cursor = shape;
        sendMessage(QJsonObject{{"t", "cursor"}, {"shape", shape}});
    }

    Surface *m_surface;
    QSocketNotifier *m_notifier;
    Qt::MouseButtons m_buttons = Qt::NoButton;
    int m_cursor = int(Qt::ArrowCursor);
};

// `--do`: steps a snapshot runs before its last shot, the way a user would
// use the surface: click (by text or position), type, press keys, wait,
// check what is shown, save pictures between steps.
struct Step {
    QString source, verb;
    QStringList args;
};

// Steps are separated by `;` or new lines; "quoted text" keeps spaces and `;`.
static QList<Step> parseSteps(const QString &text) {
    QList<Step> steps;
    QStringList tokens;
    QString token, source;
    bool quoted = false, hasToken = false;
    auto endToken = [&]() {
        if (hasToken) tokens.append(token);
        token.clear();
        hasToken = false;
    };
    auto endStep = [&]() {
        endToken();
        if (!tokens.isEmpty()) steps.append(Step{source.trimmed(), tokens.first().toLower(), tokens.mid(1)});
        tokens.clear();
        source.clear();
    };
    for (int i = 0; i < text.size(); ++i) {
        const QChar c = text.at(i);
        if (quoted) {
            source += c;
            if (c == '\\' && i + 1 < text.size()) { token += text.at(++i); source += text.at(i); }
            else if (c == '"') quoted = false;
            else token += c;
        } else if (c == '"') {
            source += c;
            quoted = true;
            hasToken = true;
        } else if (c == ';' || c == '\n') {
            endStep();
        } else if (c.isSpace()) {
            source += c;
            endToken();
        } else {
            source += c;
            token += c;
            hasToken = true;
        }
    }
    endStep();
    return steps;
}

class Script : public QObject {
    Q_OBJECT
public:
    Script(Surface *surface, const QList<Step> &steps, const QString &out, std::function<void(QJsonArray, bool)> done)
        : m_surface(surface), m_steps(steps), m_out(out), m_done(std::move(done)) {}

    void start() {
        QFocusEvent focus(QEvent::FocusIn, Qt::OtherFocusReason);
        QCoreApplication::sendEvent(m_surface->window(), &focus);
        next();
    }

private:
    void next() {
        if (m_index >= m_steps.size()) {
            m_done(m_report, true);
            return;
        }
        const Step step = m_steps.at(m_index++);
        m_surface->polish();
        QJsonObject r{{"step", step.source}};
        QString error;
        int delay = 100;  // let bindings, animations and handlers settle
        run(step, r, error, delay);
        r["ok"] = error.isEmpty();
        if (!error.isEmpty()) r["error"] = error;
        m_report.append(r);
        if (!error.isEmpty()) {
            m_done(m_report, false);
            return;
        }
        QTimer::singleShot(delay, this, &Script::next);
    }

    void run(const Step &step, QJsonObject &r, QString &error, int &delay) {
        const QString &verb = step.verb;
        const QString rest = step.args.join(' ');
        if (verb == "click" || verb == "double" || verb == "move" || verb == "press" || verb == "release") {
            QStringList args = step.args;
            Qt::MouseButton button = Qt::LeftButton;
            if (!args.isEmpty() && (args.last() == "right" || args.last() == "middle")) {
                button = args.takeLast() == "right" ? Qt::RightButton : Qt::MiddleButton;
            }
            QPointF at;
            if (!target(args.join(' '), at, r, error)) return;
            pointer(QEvent::MouseMove, at, Qt::NoButton);
            if (verb == "move") return;
            if (verb != "release") pointer(QEvent::MouseButtonPress, at, button);
            if (verb != "press") pointer(QEvent::MouseButtonRelease, at, button);
            if (verb == "double") {
                pointer(QEvent::MouseButtonDblClick, at, button);
                pointer(QEvent::MouseButtonRelease, at, button);
            }
        } else if (verb == "drag") {
            QPointF from, to;
            if (step.args.size() != 2 || !point(step.args[0], from) || !point(step.args[1], to)) {
                error = "drag needs two points: drag 10,20 200,20";
                return;
            }
            pointer(QEvent::MouseMove, from, Qt::NoButton);
            pointer(QEvent::MouseButtonPress, from, Qt::LeftButton);
            for (int i = 1; i <= 10; ++i) pointer(QEvent::MouseMove, from + (to - from) * (i / 10.0), Qt::NoButton);
            pointer(QEvent::MouseButtonRelease, to, Qt::LeftButton);
        } else if (verb == "scroll") {
            QStringList args = step.args;
            bool ok = false;
            const double amount = args.isEmpty() ? 0 : args.takeLast().toDouble(&ok);
            QPointF at;
            if (!ok) {
                error = "scroll needs a place and an amount: scroll 100,80 3 (down), scroll \"List\" -2 (up)";
                return;
            }
            if (!target(args.join(' '), at, r, error)) return;
            QWheelEvent ev(at, at, QPoint(), QPoint(0, std::lround(-amount * 120)), m_buttons, Qt::NoModifier,
                           Qt::NoScrollPhase, false);
            QCoreApplication::sendEvent(m_surface->window(), &ev);
        } else if (verb == "type") {
            for (const char32_t u : rest.toUcs4()) {
                const QString text = QString::fromUcs4(&u, 1);
                if (text == "\n") { key(Qt::Key_Return, Qt::NoModifier, "\r"); continue; }
                const QChar c = text.at(0);
                const int code = u < 128 && c.isLetter() ? c.toUpper().unicode() : (u == ' ' ? Qt::Key_Space : (u < 128 ? int(u) : Qt::Key_unknown));
                key(code, u < 128 && c.isUpper() ? Qt::ShiftModifier : Qt::NoModifier, text);
            }
        } else if (verb == "key") {
            if (step.args.isEmpty()) error = "key needs a key: key Return, key Escape, key Ctrl+A, key Down Down Return";
            for (const QString &name : step.args) {
                int code = 0;
                Qt::KeyboardModifiers mods;
                QString text;
                if (!keyCombo(name, code, mods, text)) {
                    error = QString("unknown key %1: use names like Return, Escape, Tab, Space, Backspace, Delete, "
                                    "Up, Down, Left, Right, Home, End, PageUp, F5, a, Ctrl+A, Shift+Tab").arg(name);
                    return;
                }
                key(code, mods, text);
            }
        } else if (verb == "wait") {
            static const QRegularExpression re("^(\\d+(?:\\.\\d+)?)(ms|s)?$");
            const auto m = re.match(rest.trimmed());
            if (!m.hasMatch()) {
                error = "wait needs a time: wait 500 (ms), wait 2s";
                return;
            }
            const double n = m.captured(1).toDouble();
            delay = std::clamp(int(m.captured(2) == "s" ? n * 1000 : n), 0, 60000);
        } else if (verb == "shot") {
            QString name = rest.trimmed().isEmpty() ? QString::number(++m_shots) : rest.trimmed();
            name.replace(QRegularExpression("[^A-Za-z0-9_-]"), "-");
            const QFileInfo out(m_out);
            const QString path = out.dir().filePath(out.completeBaseName() + "-" + name + ".png");
            const QImage image = m_surface->renderFrame();
            if (!image.save(path, "PNG")) error = "cannot write " + path;
            r["out"] = path;
        } else if (verb == "cardbutton") {
            // A click on one of the plugin's card buttons, on a sample card;
            // `key=value` after the id changes the card (status=working).
            if (step.args.isEmpty()) {
                error = "cardbutton needs a button id: cardbutton left, cardbutton sum status=working harness=codex";
                return;
            }
            QJsonObject card{{"id", "sd_term_sample"}, {"kind", "terminal"}, {"title", "Fix the login bug"},
                             {"harness", "claude"}, {"folder", childEnvironment().value("HOME") + "/project"},
                             {"status", "waiting"}, {"minimized", false}};
            for (const QString &pair : step.args.mid(1)) {
                const qsizetype eq = pair.indexOf('=');
                if (eq <= 0) {
                    error = QString("%1 is not key=value: cardbutton sum status=working").arg(quote(pair));
                    return;
                }
                const QString key = pair.left(eq), value = pair.mid(eq + 1);
                card[key] = key == "minimized" ? QJsonValue(value == "true") : QJsonValue(value);
            }
            m_surface->api()->deliver(QJsonObject{{"name", "cardButton"}, {"button", step.args.first()}, {"card", card}});
            r["card"] = card;
        } else if (verb == "expect" || verb == "absent") {
            if (rest.isEmpty()) {
                error = verb + " needs the text to look for";
                return;
            }
            QQuickItem *item = find(rest);
            if (verb == "expect" && !item) error = QString("no visible text %1; shown: %2").arg(quote(rest), shown());
            if (verb == "absent" && item) error = QString("%1 is shown").arg(quote(rest));
        } else {
            error = QString("unknown step %1: use click, double, move, press, release, drag, scroll, type, key, "
                            "wait, shot, expect, absent, cardbutton").arg(quote(verb));
        }
    }

    static QString quote(const QString &s) { return "\"" + s + "\""; }

    static bool point(const QString &text, QPointF &at) {
        static const QRegularExpression re("^(-?\\d+(?:\\.\\d+)?),\\s*(-?\\d+(?:\\.\\d+)?)$");
        const auto m = re.match(text.trimmed());
        if (!m.hasMatch()) return false;
        at = QPointF(m.captured(1).toDouble(), m.captured(2).toDouble());
        return true;
    }

    // A point "x,y", an object name "@name", or text shown on the surface
    // (a Text, a Button's text, a field's placeholder): its center.
    bool target(const QString &text, QPointF &at, QJsonObject &r, QString &error) {
        if (text.isEmpty()) {
            error = "say where: a point (120,40), shown text (\"OK\") or an objectName (@ok)";
            return false;
        }
        if (point(text, at)) return true;
        QQuickItem *item = text.startsWith('@') ? byName(m_surface->window()->contentItem(), text.mid(1)) : find(text);
        if (!item) {
            error = text.startsWith('@') ? QString("no visible item with objectName %1").arg(quote(text.mid(1)))
                                         : QString("no visible text %1; shown: %2").arg(quote(text), shown());
            return false;
        }
        at = item->mapToScene(QPointF(item->width() / 2, item->height() / 2));
        r["at"] = QJsonArray{std::round(at.x()), std::round(at.y())};
        return true;
    }

    bool showing(QQuickItem *item) const {
        if (!item->isVisible() || item->width() <= 0 || item->height() <= 0) return false;
        for (QQuickItem *i = item; i; i = i->parentItem())
            if (i->opacity() <= 0.01) return false;
        const QRectF box = item->mapRectToScene(QRectF(0, 0, item->width(), item->height()));
        return box.intersects(QRectF(0, 0, m_surface->window()->width(), m_surface->window()->height()));
    }

    static QString label(QQuickItem *item) {
        for (const char *name : {"text", "placeholderText", "title"}) {
            const QVariant v = item->property(name);
            if (v.typeId() == QMetaType::QString && !v.toString().trimmed().isEmpty()) return v.toString().trimmed();
        }
        return QString();
    }

    // Shown items, in drawing order; a zero-size item's children may show.
    void walk(QQuickItem *item, const std::function<void(QQuickItem *)> &visit) const {
        if (!item->isVisible() || item->opacity() <= 0.01) return;
        if (showing(item)) visit(item);
        QList<QQuickItem *> children = item->childItems();
        std::stable_sort(children.begin(), children.end(), [](QQuickItem *x, QQuickItem *y) { return x->z() < y->z(); });
        for (QQuickItem *child : children) walk(child, visit);
    }

    // The topmost shown item whose text is `text` (else contains it, any case).
    QQuickItem *find(const QString &text) const {
        QQuickItem *exact = nullptr, *partial = nullptr;
        walk(m_surface->window()->contentItem(), [&](QQuickItem *item) {
            const QString l = label(item);
            if (l.isEmpty()) return;
            if (l == text) exact = item;
            else if (l.contains(text, Qt::CaseInsensitive)) partial = item;
        });
        return exact ? exact : partial;
    }

    QQuickItem *byName(QQuickItem *item, const QString &name) const {
        QQuickItem *found = nullptr;
        walk(item, [&](QQuickItem *i) { if (i->objectName() == name) found = i; });
        return found;
    }

    QString shown() const {
        QStringList texts;
        walk(m_surface->window()->contentItem(), [&](QQuickItem *item) {
            const QString l = label(item);
            if (!l.isEmpty() && !texts.contains(quote(l)) && texts.size() < 30) texts.append(quote(l.left(60)));
        });
        return texts.isEmpty() ? "no text" : texts.join(", ");
    }

    void pointer(QEvent::Type type, QPointF at, Qt::MouseButton button) {
        QQuickWindow *win = m_surface->window();
        if (!m_entered) {
            QEnterEvent enter(at, at, at);
            QCoreApplication::sendEvent(win, &enter);
            m_entered = true;
        }
        if (type == QEvent::MouseButtonPress) m_buttons |= button;
        if (type == QEvent::MouseButtonRelease) m_buttons &= ~button;
        QMouseEvent ev(type, at, at, at, button, m_buttons, Qt::NoModifier);
        QCoreApplication::sendEvent(win, &ev);
    }

    void key(int code, Qt::KeyboardModifiers mods, const QString &text) {
        QKeyEvent press(QEvent::KeyPress, code, mods, text);
        QCoreApplication::sendEvent(m_surface->window(), &press);
        QKeyEvent release(QEvent::KeyRelease, code, mods, text);
        QCoreApplication::sendEvent(m_surface->window(), &release);
    }

    static bool keyCombo(QString name, int &code, Qt::KeyboardModifiers &mods, QString &text) {
        static const QHash<QString, QString> aliases{
            {"escape", "Esc"}, {"enter", "Return"}, {"delete", "Del"}, {"pageup", "PgUp"}, {"pagedown", "PgDown"},
            {"page_up", "PgUp"}, {"page_down", "PgDown"}, {"control", "Ctrl"}, {"super", "Meta"}};
        QStringList parts = name.split('+');
        for (QString &p : parts) {
            const auto alias = aliases.find(p.toLower());
            if (alias != aliases.end()) p = alias.value();
        }
        const QKeySequence seq = QKeySequence::fromString(parts.join('+'), QKeySequence::PortableText);
        if (seq.count() != 1 || seq[0].key() == Qt::Key_unknown) return false;
        code = seq[0].key();
        mods = seq[0].keyboardModifiers();
        const bool plain = !(mods & (Qt::ControlModifier | Qt::AltModifier | Qt::MetaModifier));
        if (code == Qt::Key_Return) text = "\r";
        else if (code == Qt::Key_Tab) text = "\t";
        else if (code == Qt::Key_Space && plain) text = " ";
        else if (plain && code >= 0x21 && code <= 0x7e) {
            const QString last = parts.last();
            text = last.size() == 1 ? (mods & Qt::ShiftModifier ? last.toUpper() : last) : QString();
        }
        return true;
    }

    Surface *m_surface;
    QList<Step> m_steps;
    QString m_out;
    std::function<void(QJsonArray, bool)> m_done;
    QJsonArray m_report;
    int m_index = 0, m_shots = 0;
    bool m_entered = false;
    Qt::MouseButtons m_buttons = Qt::NoButton;
};

struct Args {
    QString mode, qml, pluginDir, pluginId, surfaceId, instanceId, out, theme, data, steps;
    int fd = -1, width = 0, height = 0, waitMs = 300;
    qreal scale = 1.0;
};

static void usage() {
    std::fprintf(stderr,
                 "usage:\n"
                 "  sd-qml-host --check FILE.qml [--plugin-dir DIR]\n"
                 "  sd-qml-host --snapshot FILE.qml --out FILE.png --width W --height H "
                 "[--scale S] [--theme JSON] [--wait MS] [--data FILE] [--do STEPS]\n"
                 "  sd-qml-host --serve --fd N --qml FILE.qml --plugin-dir DIR --width W --height H [--data FILE]\n");
}

static bool parseArgs(int argc, char **argv, Args &a) {
    for (int i = 1; i < argc; ++i) {
        const QString arg = QString::fromLocal8Bit(argv[i]);
        auto next = [&](QString &out) {
            if (i + 1 >= argc) return false;
            out = QString::fromLocal8Bit(argv[++i]);
            return true;
        };
        QString v;
        if (arg == "--check" || arg == "--snapshot") {
            a.mode = arg.mid(2);
            if (!next(a.qml)) return false;
        } else if (arg == "--serve") {
            a.mode = "serve";
        } else if (arg == "--qml") {
            if (!next(a.qml)) return false;
        } else if (arg == "--plugin-dir") {
            if (!next(a.pluginDir)) return false;
        } else if (arg == "--plugin-id") {
            if (!next(a.pluginId)) return false;
        } else if (arg == "--surface-id") {
            if (!next(a.surfaceId)) return false;
        } else if (arg == "--instance-id") {
            if (!next(a.instanceId)) return false;
        } else if (arg == "--out") {
            if (!next(a.out)) return false;
        } else if (arg == "--data") {
            if (!next(a.data)) return false;
        } else if (arg == "--do") {
            if (!next(v)) return false;
            a.steps += (a.steps.isEmpty() ? "" : ";") + v;
        } else if (arg == "--theme") {
            if (!next(a.theme)) return false;
        } else if (arg == "--fd" && next(v)) {
            a.fd = v.toInt();
        } else if (arg == "--width" && next(v)) {
            a.width = v.toInt();
        } else if (arg == "--height" && next(v)) {
            a.height = v.toInt();
        } else if (arg == "--scale" && next(v)) {
            a.scale = v.toDouble();
        } else if (arg == "--wait" && next(v)) {
            a.waitMs = v.toInt();
        } else {
            return false;
        }
    }
    if (a.mode.isEmpty() || a.qml.isEmpty()) return false;
    if (a.pluginDir.isEmpty()) a.pluginDir = QFileInfo(a.qml).absolutePath();
    if (a.pluginId.isEmpty()) a.pluginId = QFileInfo(a.pluginDir).fileName();
    if (a.width <= 0) a.width = 400;
    if (a.height <= 0) a.height = 300;
    if (a.mode == "snapshot" && a.out.isEmpty()) return false;
    if (a.mode == "serve" && a.fd < 0) return false;
    return true;
}

int main(int argc, char **argv) {
    Args a;
    if (!parseArgs(argc, argv, a)) {
        usage();
        return 64;
    }
    // Programs a plugin starts get the environment from before the changes below.
    childEnvironment();
    // Off screen, software rendering: no window, no GPU context, the same
    // pixels on every machine. The host never opens a window of its own.
    qputenv("QT_QPA_PLATFORM", "offscreen");
    qputenv("QT_QUICK_BACKEND", "software");
    // A desktop platform theme (gtk3, qt6ct, kde) wants a display, which the
    // host does not have, and would make plugins look different per machine.
    qunsetenv("QT_QPA_PLATFORMTHEME");
    qunsetenv("QT_STYLE_OVERRIDE");
    if (qEnvironmentVariableIsEmpty("QT_QUICK_CONTROLS_STYLE")) qputenv("QT_QUICK_CONTROLS_STYLE", "Basic");
    if (a.scale > 0 && a.scale != 1.0)
        qputenv("QT_SCALE_FACTOR", QByteArray::number(a.scale));
    QQuickWindow::setGraphicsApi(QSGRendererInterface::Software);
    QGuiApplication app(argc, argv);
    app.setApplicationName("sd-qml-host");

    if (a.mode == "serve") {
        g_fd = a.fd;
        ::fcntl(g_fd, F_SETFD, FD_CLOEXEC);
    }
    const QVariantMap theme = parseTheme(a.theme);
    Surface surface(QFileInfo(a.qml).absoluteFilePath(), QFileInfo(a.pluginDir).absoluteFilePath(),
                    a.pluginId, a.surfaceId, a.mode == "serve", theme);
    surface.api()->setDataFile(a.data);
    surface.api()->setInstanceId(a.instanceId);
    const QJsonArray errors = surface.load();

    if (a.mode == "check") {
        QJsonObject r{{"ok", errors.isEmpty()}, {"errors", errors}, {"warnings", surface.warnings()}};
        std::printf("%s\n", QJsonDocument(r).toJson(QJsonDocument::Compact).constData());
        return errors.isEmpty() ? 0 : 1;
    }

    if (!errors.isEmpty()) {
        if (a.mode == "serve") {
            sendMessage(QJsonObject{{"t", "error"}, {"errors", errors}});
        } else {
            QJsonObject r{{"ok", false}, {"errors", errors}};
            std::printf("%s\n", QJsonDocument(r).toJson(QJsonDocument::Compact).constData());
        }
        return 1;
    }

    surface.resize(a.width, a.height);

    if (a.mode == "snapshot") {
        const QList<Step> steps = parseSteps(a.steps);
        // The last shot, and what the plugin did, asked for and saved.
        auto finish = [&](const QJsonArray &stepReport, bool stepsOk) {
            QImage image = surface.renderFrame();
            const bool saved = image.save(a.out, "PNG");
            // An all-transparent frame is almost always a mistake (zero size,
            // no color, everything outside the root): say so.
            qint64 painted = 0;
            for (int y = 0; y < image.height(); ++y) {
                const QRgb *line = reinterpret_cast<const QRgb *>(image.constScanLine(y));
                for (int x = 0; x < image.width(); ++x) painted += qAlpha(line[x]) != 0;
            }
            QJsonObject r{{"ok", saved && stepsOk}, {"out", a.out}, {"width", image.width()},
                          {"height", image.height()},
                          {"paintedFraction", double(painted) / std::max<qint64>(1, qint64(image.width()) * image.height())},
                          {"warnings", surface.warnings()}};
            if (!steps.isEmpty()) {
                r["steps"] = stepReport;
                r["values"] = surface.api()->savedValues();
            }
            if (!surface.api()->calls().isEmpty()) r["calls"] = surface.api()->calls();
            if (!surface.api()->runs().isEmpty()) r["runs"] = surface.api()->runs();
            if (!surface.api()->logs().isEmpty()) r["logs"] = QJsonArray::fromStringList(surface.api()->logs());
            std::printf("%s\n", QJsonDocument(r).toJson(QJsonDocument::Compact).constData());
            app.exit(saved && stepsOk ? 0 : 1);
        };
        Script script(&surface, steps, a.out, finish);
        // Let timers, bindings and short animations settle before the shot.
        QTimer::singleShot(std::max(0, a.waitMs), &app, [&]() {
            if (steps.isEmpty()) finish(QJsonArray(), true);
            else script.start();
        });
        return app.exec();
    }

    Connection connection(&surface);
    sendMessage(QJsonObject{{"t", "ready"}, {"api", 1}});
    surface.scheduleRender();
    return app.exec();
}

#include "main.moc"
