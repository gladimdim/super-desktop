import QtQuick
import QtQuick.Layouts

// Snake on a card: a Timer is the game loop, Keys steer, the best score is
// saved with sd.setValue. The card has the keyboard once it is clicked.
Rectangle {
    id: game
    color: sd.theme.background
    focus: true

    readonly property int cells: 20
    property var snake: []          // [{x, y}], head first
    property var food: ({ x: 5, y: 5 })
    property point direction: Qt.point(1, 0)
    property point nextDirection: Qt.point(1, 0)
    property int score: 0
    property bool over: false
    property bool paused: true

    function reset() {
        snake = [{ x: 10, y: 10 }, { x: 9, y: 10 }, { x: 8, y: 10 }]
        direction = Qt.point(1, 0)
        nextDirection = direction
        score = 0
        over = false
        placeFood()
    }

    function placeFood() {
        for (;;) {
            const spot = { x: Math.floor(Math.random() * cells), y: Math.floor(Math.random() * cells) }
            if (!snake.some(p => p.x === spot.x && p.y === spot.y)) { food = spot; return }
        }
    }

    function step() {
        direction = nextDirection
        const head = { x: (snake[0].x + direction.x + cells) % cells, y: (snake[0].y + direction.y + cells) % cells }
        if (snake.some(p => p.x === head.x && p.y === head.y)) {
            over = true
            if (score > (sd.values.best || 0)) sd.setValue("best", score)
            return
        }
        const grown = head.x === food.x && head.y === food.y
        const body = [head].concat(snake)
        if (grown) { score += 1; placeFood() } else body.pop()
        snake = body
    }

    Component.onCompleted: reset()

    Timer {
        interval: Math.max(60, 160 - game.score * 4)
        running: game.activeFocus && !game.over && !game.paused
        repeat: true
        onTriggered: game.step()
    }

    Keys.onPressed: (event) => {
        const turn = (x, y) => {
            // No reversing into yourself.
            if (game.direction.x !== -x || game.direction.y !== -y) game.nextDirection = Qt.point(x, y)
        }
        switch (event.key) {
        case Qt.Key_Up: case Qt.Key_W: turn(0, -1); break
        case Qt.Key_Down: case Qt.Key_S: turn(0, 1); break
        case Qt.Key_Left: case Qt.Key_A: turn(-1, 0); break
        case Qt.Key_Right: case Qt.Key_D: turn(1, 0); break
        case Qt.Key_Space: if (game.over) game.reset(); else game.paused = !game.paused; break
        default: return
        }
        if (game.paused && event.key !== Qt.Key_Space) game.paused = false
        event.accepted = true
    }

    MouseArea { anchors.fill: parent; cursorShape: Qt.PointingHandCursor; onClicked: game.forceActiveFocus() }

    ColumnLayout {
        anchors.fill: parent
        anchors.margins: 12
        spacing: 8

        RowLayout {
            Layout.fillWidth: true
            Text { text: "Score " + game.score; color: sd.theme.foreground; font.family: sd.theme.fontFamily; font.pixelSize: 16 }
            Item { Layout.fillWidth: true }
            Text { text: "Best " + (sd.values.best || 0); color: Qt.alpha(sd.theme.foreground, 0.7); font.family: sd.theme.fontFamily; font.pixelSize: 16 }
        }

        Rectangle {
            id: board
            Layout.fillWidth: true
            Layout.fillHeight: true
            color: sd.theme.darkBackground
            radius: 6
            readonly property real cell: Math.min(width, height) / game.cells

            Item {
                width: board.cell * game.cells
                height: width
                anchors.centerIn: parent

                Rectangle {
                    x: game.food.x * board.cell + 2; y: game.food.y * board.cell + 2
                    width: board.cell - 4; height: width; radius: width / 2
                    color: sd.theme.red
                }
                Repeater {
                    model: game.snake
                    delegate: Rectangle {
                        x: modelData.x * board.cell + 1; y: modelData.y * board.cell + 1
                        width: board.cell - 2; height: width; radius: 3
                        color: index === 0 ? sd.theme.accent : sd.theme.green
                    }
                }
            }

            Text {
                anchors.centerIn: parent
                visible: game.over || game.paused || !game.activeFocus
                text: game.over ? "Game over — Space to play again"
                    : !game.activeFocus ? "Click to play"
                    : "Arrow keys to start, Space to pause"
                color: sd.theme.foreground
                font.family: sd.theme.fontFamily
                font.pixelSize: 15
                style: Text.Outline
                styleColor: sd.theme.background
            }
        }
    }
}
