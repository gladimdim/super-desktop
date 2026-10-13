import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// The panel: start or stop the timer. The end time is saved with
// sd.setValue, so the top-bar item (another surface) follows it.
Rectangle {
    id: root
    color: sd.theme.background

    property int minutes: sd.values.minutes || 25
    property bool running: (sd.values.endsAt || 0) > Date.now()

    // Re-read `running` every second; a binding on Date.now() alone would not.
    Timer { interval: 1000; running: true; repeat: true; onTriggered: root.running = (sd.values.endsAt || 0) > Date.now() }

    ColumnLayout {
        anchors.centerIn: parent
        spacing: 14

        Text {
            Layout.alignment: Qt.AlignHCenter
            text: root.running ? "Focusing…" : root.minutes + " minutes"
            color: sd.theme.foreground
            font.family: sd.theme.fontFamily
            font.pixelSize: 22
        }
        Button {
            Layout.alignment: Qt.AlignHCenter
            text: root.running ? "Stop" : "Start"
            onClicked: {
                if (root.running) {
                    sd.setValue("endsAt", 0)
                } else {
                    sd.setValue("endsAt", Date.now() + root.minutes * 60 * 1000)
                    sd.call("notify", { title: "Pomodoro", body: root.minutes + " minutes of focus started" })
                }
                root.running = !root.running
            }
        }
    }
}
