import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// Its section in Settings → Plugins: the timer's length.
Item {
    RowLayout {
        anchors.left: parent.left
        anchors.verticalCenter: parent.verticalCenter
        spacing: 10

        Text {
            text: "Minutes per session"
            color: sd.theme.foreground
            font.family: sd.theme.fontFamily
        }
        SpinBox {
            from: 5; to: 90; stepSize: 5
            value: sd.values.minutes || 25
            onValueModified: sd.setValue("minutes", value)
        }
    }
}
