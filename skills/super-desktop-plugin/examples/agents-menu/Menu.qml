import QtQuick
import QtQuick.Layouts

// The popup: one row per agent. A click opens a card for it and closes the
// popup (popup.close); the popup runs only while it is open.
Rectangle {
    id: root
    color: sd.theme.background

    property var harnesses: []
    property string error: ""

    Component.onCompleted: sd.call("harness.list", {}, function(result, err) {
        if (err) root.error = err.message
        else root.harnesses = result
    })

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 6
        spacing: 2

        Repeater {
            model: root.harnesses
            delegate: Rectangle {
                id: row
                required property var modelData
                Layout.fillWidth: true
                Layout.preferredHeight: 32
                radius: 8
                color: area.containsMouse ? Qt.alpha(sd.theme.accent, 0.25) : "transparent"

                RowLayout {
                    anchors.fill: parent
                    anchors.leftMargin: 10
                    anchors.rightMargin: 10
                    spacing: 8
                    Image {
                        Layout.preferredWidth: 16
                        Layout.preferredHeight: 16
                        visible: row.modelData.icon.length > 0
                        source: row.modelData.icon.length > 0 ? "file://" + row.modelData.icon : ""
                        sourceSize.width: 16
                        sourceSize.height: 16
                    }
                    Text {
                        Layout.fillWidth: true
                        text: row.modelData.name
                        color: sd.theme.foreground
                        font.family: sd.theme.fontFamily
                        elide: Text.ElideRight
                    }
                }
                MouseArea {
                    id: area
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: sd.call("harness.launch", { harness: row.modelData.id }, function(r, err) {
                        if (err) root.error = err.message
                        else sd.call("popup.close", { surface: sd.surfaceId })
                    })
                }
            }
        }

        Text {
            Layout.fillWidth: true
            Layout.margins: 6
            visible: root.error.length > 0 || !sd.connected
            text: sd.connected ? root.error : "Your agents appear here inside SUPER DESKTOP."
            color: sd.connected ? sd.theme.red : sd.theme.muted
            font.family: sd.theme.fontFamily
            wrapMode: Text.Wrap
        }

        Item { Layout.fillHeight: true }
    }
}
