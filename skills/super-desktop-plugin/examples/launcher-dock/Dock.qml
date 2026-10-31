import QtQuick
import QtQuick.Layouts

// One button per AI agent offered in the top bar, and a hide button.
Rectangle {
    id: root
    color: Qt.alpha(sd.theme.background, 0.85)
    radius: 14
    border.color: Qt.alpha(sd.theme.accent, 0.5)
    border.width: 1

    property var harnesses: []
    property string error: ""

    function refresh() {
        sd.call("harness.list", {}, function(result, err) {
            if (err) { root.error = err.message; return }
            root.error = ""
            root.harnesses = result
        })
    }

    Component.onCompleted: refresh()
    // The user may change the agents in Settings while the overlay is hidden.
    Connections {
        target: sd
        function onVisibleChanged(visible) { if (visible) root.refresh() }
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 8
        spacing: 8

        Repeater {
            model: root.harnesses
            delegate: DockButton {
                Layout.fillWidth: true
                icon: modelData.icon
                glyph: modelData.name.charAt(0)
                onClicked: sd.call("harness.launch", { harness: modelData.id }, function(r, err) {
                    if (err) root.error = err.message
                })
            }
        }

        Text {
            Layout.fillWidth: true
            visible: root.error.length > 0 || !sd.connected
            text: sd.connected ? root.error : "Agents appear here inside SUPER DESKTOP."
            color: sd.connected ? sd.theme.red : Qt.alpha(sd.theme.foreground, 0.7)
            wrapMode: Text.Wrap
            horizontalAlignment: Text.AlignHCenter
            font.pixelSize: 10
        }

        Item { Layout.fillHeight: true }

        DockButton {
            Layout.fillWidth: true
            glyph: "⏷"
            onClicked: sd.call("overlay.hide")
        }
    }
}
