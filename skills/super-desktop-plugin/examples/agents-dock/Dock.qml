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
    property var summaries: ({})      // card id → one line, or "…" while asked

    // The end of the card's terminal, summarized in one line by the AI.
    function summarize(card) {
        const next = Object.assign({}, summaries)
        next[card.id] = "…"
        summaries = next
        sd.call("cards.text", { id: card.id, lines: 80 }, function (screen, err) {
            if (err) { setSummary(card.id, err.message); return }
            sd.call("llm.complete", {
                system: "You summarize what a coding agent in a terminal is doing.",
                prompt: "In one short sentence, what is this agent doing or waiting for?\n\n" + screen.text,
                maxTokens: 60,
                fast: true,
            }, function (answer, err2) { setSummary(card.id, err2 ? err2.message : answer.text) })
        })
    }

    function setSummary(id, text) {
        const next = Object.assign({}, summaries)
        next[id] = text
        summaries = next
    }

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
            color: Qt.alpha(sd.theme.foreground, 0.7)
            wrapMode: Text.Wrap
            font.pixelSize: 11
        }

        Repeater {
            model: root.cards
            delegate: Rectangle {
                Layout.fillWidth: true
                Layout.preferredHeight: 46 + (summary.visible ? summary.implicitHeight + 4 : 0)
                radius: 8
                color: area.containsMouse ? Qt.alpha(sd.theme.accent, 0.25) : Qt.alpha(sd.theme.surface, 0.8)

                MouseArea {
                    id: area
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: sd.call("cards.focus", { id: modelData.id })
                }

                RowLayout {
                    id: row
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.top: parent.top
                    anchors.margins: 8
                    height: 30
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
                            color: Qt.alpha(sd.theme.foreground, 0.7)
                            elide: Text.ElideRight
                            font.pixelSize: 10
                        }
                    }
                    Text {
                        text: "✦"
                        color: sparkle.containsMouse ? sd.theme.accent : Qt.alpha(sd.theme.foreground, 0.7)
                        font.pixelSize: 14
                        MouseArea {
                            id: sparkle
                            anchors.fill: parent
                            anchors.margins: -6
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: root.summarize(modelData)
                        }
                    }
                }
                Text {
                    id: summary
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.top: row.bottom
                    anchors.margins: 8
                    anchors.topMargin: 2
                    visible: (root.summaries[modelData.id] || "").length > 0
                    text: root.summaries[modelData.id] || ""
                    color: sd.theme.foreground
                    wrapMode: Text.Wrap
                    font.pixelSize: 10
                    font.italic: true
                }
            }
        }
        Item { Layout.fillHeight: true }
    }
}
