import QtQuick
import QtQuick.Layouts

// Every terminal card with its status; refreshed every two seconds while the
// overlay is shown. A click brings that card to the front with the keyboard.
Rectangle {
    id: root
    color: Qt.alpha(sd.theme.background, 0.85)
    radius: 14
    border.color: Qt.alpha(sd.theme.accent, 0.5)
    border.width: 1

    property var cards: []
    property bool shown: true

    function refresh() {
        sd.call("cards.list", {}, function (result, err) {
            if (!err) root.cards = result.filter(c => c.kind === "terminal")
        })
    }

    function statusColor(status) {
        switch (status) {
        case "working": return sd.theme.yellow
        case "waiting": return sd.theme.orange
        case "finished": return sd.theme.green
        case "error": case "exited": return sd.theme.red
        default: return sd.theme.muted
        }
    }

    Component.onCompleted: refresh()
    Timer { interval: 2000; repeat: true; running: root.shown; onTriggered: root.refresh() }
    Connections {
        target: sd
        function onVisibleChanged(visible) { root.shown = visible; if (visible) root.refresh() }
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 10
        spacing: 6

        Text {
            text: "Agents"
            color: sd.theme.accent
            font.family: sd.theme.fontFamily
            font.pixelSize: 14
            font.bold: true
        }
        Text {
            visible: root.cards.length === 0
            Layout.fillWidth: true
            text: sd.connected ? "No agent cards." : "Agent cards appear here inside SUPER DESKTOP."
            color: sd.theme.muted
            wrapMode: Text.Wrap
            font.pixelSize: 11
        }

        Repeater {
            model: root.cards
            delegate: Rectangle {
                Layout.fillWidth: true
                Layout.preferredHeight: 46
                radius: 8
                color: area.containsMouse ? Qt.alpha(sd.theme.accent, 0.25) : Qt.alpha(sd.theme.surface, 0.8)

                RowLayout {
                    anchors.fill: parent
                    anchors.margins: 8
                    spacing: 8
                    Rectangle {
                        width: 10; height: 10; radius: 5
                        color: root.statusColor(modelData.status)
                    }
                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: 0
                        Text {
                            Layout.fillWidth: true
                            text: modelData.title || modelData.harness
                            color: sd.theme.foreground
                            elide: Text.ElideRight
                            font.pixelSize: 12
                        }
                        Text {
                            Layout.fillWidth: true
                            text: modelData.status + (modelData.folder ? " · " + modelData.folder.split("/").pop() : "")
                            color: sd.theme.muted
                            elide: Text.ElideRight
                            font.pixelSize: 10
                        }
                    }
                }
                MouseArea {
                    id: area
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: sd.call("cards.focus", { id: modelData.id })
                }
            }
        }
        Item { Layout.fillHeight: true }
    }
}
