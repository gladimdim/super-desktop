import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import "reminders.js" as R

// The right-click popup under the bell: every reminder, soonest first, like
// a context menu. Clicking one removes it.
Rectangle {
    id: root
    color: sd.theme.background

    // Text colors checked against the theme's background (reminders.js).
    readonly property color textSoft: R.soft(sd.theme, 0.3)
    readonly property color textHint: R.soft(sd.theme, 0.45)
    readonly property color accentText: R.readable(sd.theme, ["accent", "blue", "cyan"], 4.5)
    readonly property color doneText: R.readable(sd.theme, ["yellow", "orange", "accent"], 4.5)
    readonly property color dangerText: R.readable(sd.theme, ["red", "orange", "magenta"], 4.5)

    readonly property var reminders: R.sorted(sd.values)
    // Refreshes the "in 5 min" texts.
    property real now: Date.now()
    Timer { interval: 30000; running: true; repeat: true; onTriggered: root.now = Date.now() }

    function remove(id) {
        sd.setValue("reminders", R.list(sd.values).filter(r => r.id !== id))
    }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 6
        spacing: 4

        Text {
            Layout.fillWidth: true
            Layout.leftMargin: 8
            Layout.topMargin: 4
            text: root.reminders.length === 0 ? "Reminders" : "Reminders · click one to remove it"
            color: root.textHint
            font.family: sd.theme.fontFamily
            font.pixelSize: 11
            elide: Text.ElideRight
        }

        ListView {
            id: listView
            Layout.fillWidth: true
            Layout.fillHeight: true
            clip: true
            spacing: 2
            model: root.reminders
            ScrollBar.vertical: ScrollBar {}

            // Empty state. Children of a ListView live in its content item:
            // place by hand.
            ColumnLayout {
                visible: root.reminders.length === 0
                x: (listView.width - width) / 2
                y: (listView.height - height) / 2
                spacing: 8
                Bell {
                    Layout.alignment: Qt.AlignHCenter
                    Layout.preferredWidth: 32; Layout.preferredHeight: 32
                    color: root.textHint
                }
                Text {
                    Layout.alignment: Qt.AlignHCenter
                    text: "No reminders.\nClick the bell to add one."
                    horizontalAlignment: Text.AlignHCenter
                    color: root.textSoft
                    font.family: sd.theme.fontFamily
                    font.pixelSize: 12
                }
            }

            delegate: Rectangle {
                id: row
                required property var modelData
                readonly property bool done: modelData.fired || modelData.at <= root.now
                width: listView.width
                height: content.implicitHeight + 12
                radius: 8
                color: rowArea.containsMouse ? Qt.alpha(root.dangerText, 0.18) : "transparent"

                RowLayout {
                    id: content
                    anchors.left: parent.left
                    anchors.right: parent.right
                    anchors.verticalCenter: parent.verticalCenter
                    anchors.leftMargin: 8
                    anchors.rightMargin: 8
                    spacing: 8

                    Bell {
                        Layout.preferredWidth: 14
                        Layout.preferredHeight: 14
                        Layout.alignment: Qt.AlignTop
                        Layout.topMargin: 2
                        color: row.done ? root.doneText : root.accentText
                    }
                    ColumnLayout {
                        Layout.fillWidth: true
                        spacing: 1
                        Text {
                            Layout.fillWidth: true
                            text: row.modelData.text
                            color: sd.theme.foreground
                            font.family: sd.theme.fontFamily
                            font.pixelSize: 13
                            wrapMode: Text.Wrap
                            maximumLineCount: 2
                            elide: Text.ElideRight
                        }
                        Text {
                            Layout.fillWidth: true
                            text: R.whenText(row.modelData.at) + " · " + (row.done ? "done" : R.relativeText(row.modelData.at))
                            color: row.done ? root.doneText : root.textSoft
                            font.family: sd.theme.fontFamily
                            font.pixelSize: 11
                        }
                    }
                    Text {
                        text: "✕"
                        opacity: rowArea.containsMouse ? 1 : 0
                        color: root.dangerText
                        font.pixelSize: 13
                    }
                }
                MouseArea {
                    id: rowArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: Qt.PointingHandCursor
                    onClicked: root.remove(row.modelData.id)
                }
            }
        }
    }
}
