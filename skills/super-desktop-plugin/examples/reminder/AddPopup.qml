import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import "reminders.js" as R

// The popup under the bell: "When" (date + time, or a quick preset) and
// "What". OK (or Enter) saves it; Cancel, Esc or a click outside drops it.
Rectangle {
    id: root
    color: sd.theme.background

    // Text colors checked against the theme's background (reminders.js).
    readonly property color textSoft: R.soft(sd.theme, 0.3)
    readonly property color textHint: R.soft(sd.theme, 0.45)
    readonly property color accentText: R.readable(sd.theme, ["accent", "blue", "cyan"], 4.5)
    readonly property color doneText: R.readable(sd.theme, ["yellow", "orange", "accent"], 4.5)
    readonly property color dangerText: R.readable(sd.theme, ["red", "orange", "magenta"], 4.5)

    property string error: ""

    function setWhen(ms) {
        var d = new Date(ms)
        dateField.text = R.dateText(d)
        timeField.text = R.timeText(d)
        root.error = ""
    }
    function inMinutes(m) {
        var d = new Date(Date.now() + m * 60000)
        d.setSeconds(0, 0)
        root.setWhen(d.getTime())
    }
    function tomorrowAt(h) {
        var d = new Date()
        d.setDate(d.getDate() + 1)
        d.setHours(h, 0, 0, 0)
        root.setWhen(d.getTime())
    }
    readonly property real at: R.parse(dateField.text, timeField.text)

    function save() {
        var text = whatField.text.trim()
        if (text.length === 0) { root.error = "Write what to remind you about."; whatField.forceActiveFocus(); return }
        if (isNaN(root.at)) { root.error = "Use YYYY-MM-DD and HH:MM."; return }
        if (root.at <= Date.now()) { root.error = "That time has already passed."; return }
        var all = R.list(sd.values)
        all.push({ id: R.newId(), at: root.at, text: text, fired: false })
        sd.setValue("reminders", all)
        sd.call("popup.close", { surface: sd.surfaceId })
    }

    // A new reminder starts 10 minutes from now.
    Component.onCompleted: { inMinutes(10); whatField.forceActiveFocus() }

    component Label: Text {
        color: root.textHint
        font.family: sd.theme.fontFamily
        font.pixelSize: 12
    }
    component Preset: Button {
        property real minutes: 0
        flat: true
        padding: 4
        leftPadding: 8; rightPadding: 8
        font.family: sd.theme.fontFamily
        font.pixelSize: 12
        background: Rectangle {
            radius: height / 2
            color: parent.hovered ? Qt.alpha(sd.theme.accent, 0.35) : sd.theme.surface
            border.color: Qt.alpha(sd.theme.foreground, 0.2)
        }
        contentItem: Text {
            text: parent.text
            font: parent.font
            color: sd.theme.foreground
            horizontalAlignment: Text.AlignHCenter
        }
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 14
        spacing: 8

        Text {
            text: "New reminder"
            color: sd.theme.foreground
            font.family: sd.theme.fontFamily
            font.pixelSize: 15
            font.bold: true
        }

        Label { text: "WHEN" }
        Flow {
            Layout.fillWidth: true
            spacing: 6
            Preset { text: "10 min"; onClicked: root.inMinutes(10) }
            Preset { text: "30 min"; onClicked: root.inMinutes(30) }
            Preset { text: "1 hour"; onClicked: root.inMinutes(60) }
            Preset { text: "3 hours"; onClicked: root.inMinutes(180) }
            Preset { text: "Tomorrow 9:00"; onClicked: root.tomorrowAt(9) }
        }
        RowLayout {
            Layout.fillWidth: true
            spacing: 8
            TextField {
                id: dateField
                Layout.fillWidth: true
                placeholderText: "YYYY-MM-DD"
                placeholderTextColor: root.textHint
                font.family: sd.theme.fontFamily
                inputMethodHints: Qt.ImhDate
                onTextEdited: root.error = ""
                onAccepted: root.save()
            }
            TextField {
                id: timeField
                Layout.preferredWidth: 90
                placeholderText: "HH:MM"
                placeholderTextColor: root.textHint
                font.family: sd.theme.fontFamily
                inputMethodHints: Qt.ImhTime
                onTextEdited: root.error = ""
                onAccepted: root.save()
            }
        }
        Text {
            Layout.fillWidth: true
            text: isNaN(root.at) ? "" : R.whenText(root.at) + " · " + R.relativeText(root.at)
            color: root.accentText
            font.family: sd.theme.fontFamily
            font.pixelSize: 12
            elide: Text.ElideRight
        }

        Label { text: "WHAT"; Layout.topMargin: 4 }
        TextField {
            id: whatField
            Layout.fillWidth: true
            placeholderText: "Call mom, stretch, check the build…"
            placeholderTextColor: root.textHint
            font.family: sd.theme.fontFamily
            onTextEdited: root.error = ""
            onAccepted: root.save()
        }

        Item { Layout.fillHeight: true }

        RowLayout {
            Layout.fillWidth: true
            spacing: 8
            Text {
                Layout.fillWidth: true
                text: root.error
                color: root.dangerText
                font.family: sd.theme.fontFamily
                font.pixelSize: 12
                wrapMode: Text.Wrap
            }
            Button {
                text: "Cancel"
                font.family: sd.theme.fontFamily
                onClicked: sd.call("popup.close", { surface: sd.surfaceId })
            }
            Button {
                text: "OK"
                font.family: sd.theme.fontFamily
                onClicked: root.save()
            }
        }
    }
}
