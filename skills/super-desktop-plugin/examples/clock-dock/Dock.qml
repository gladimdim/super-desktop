import QtQuick
import QtQuick.Layouts

// A narrow dock: the time, the date and the battery level.
Rectangle {
    id: root
    color: Qt.alpha(sd.theme.background, 0.85)
    radius: 14
    border.color: Qt.alpha(sd.theme.accent, 0.5)
    border.width: 1

    property date now: new Date()
    property string battery: ""

    Timer {
        interval: 1000; running: true; repeat: true; triggeredOnStart: true
        onTriggered: root.now = new Date()
    }

    // Reads the first battery's capacity; shows nothing on a desktop PC.
    Timer {
        interval: 60000; running: true; repeat: true; triggeredOnStart: true
        onTriggered: sd.exec(["sh", "-c", "cat /sys/class/power_supply/BAT*/capacity 2>/dev/null | head -1"],
                             function(r) { root.battery = r.stdout.trim() })
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 8
        spacing: 4

        Text {
            Layout.alignment: Qt.AlignHCenter
            text: Qt.formatTime(root.now, "hh")
            color: sd.theme.accent
            font.family: sd.theme.fontFamily
            font.pixelSize: 26
            font.bold: true
        }
        Text {
            Layout.alignment: Qt.AlignHCenter
            text: Qt.formatTime(root.now, "mm")
            color: sd.theme.foreground
            font.family: sd.theme.fontFamily
            font.pixelSize: 26
        }
        Rectangle {
            Layout.alignment: Qt.AlignHCenter
            Layout.preferredWidth: parent.width * 0.5
            Layout.preferredHeight: 1
            color: sd.theme.muted
        }
        Text {
            Layout.alignment: Qt.AlignHCenter
            text: Qt.formatDate(root.now, "ddd")
            color: sd.theme.muted
            font.family: sd.theme.fontFamily
            font.pixelSize: 12
        }
        Text {
            Layout.alignment: Qt.AlignHCenter
            text: Qt.formatDate(root.now, "d MMM")
            color: sd.theme.foreground
            font.family: sd.theme.fontFamily
            font.pixelSize: 12
        }

        Item { Layout.fillHeight: true }

        Text {
            Layout.alignment: Qt.AlignHCenter
            visible: root.battery.length > 0
            text: "🔋\n" + root.battery + "%"
            horizontalAlignment: Text.AlignHCenter
            color: Number(root.battery) < 20 ? sd.theme.red : sd.theme.foreground
            font.family: sd.theme.fontFamily
            font.pixelSize: 12
        }
    }
}
