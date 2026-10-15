import QtQuick

// ⧉ in the top bar puts the open terminal cards side by side. The card
// buttons of plugin.json ("cardButtons") arrive here too: a top-bar item
// always runs, so it can take their clicks.
Rectangle {
    id: root
    radius: height / 2
    color: area.containsMouse ? sd.theme.accent : Qt.alpha(sd.theme.surface, 0.9)

    readonly property int gap: 12

    Text {
        anchors.centerIn: parent
        text: "⧉"
        color: area.containsMouse ? sd.theme.background : sd.theme.foreground
        font.family: sd.theme.fontFamily
        font.pixelSize: Math.max(12, root.height * 0.5)
    }
    MouseArea {
        id: area
        anchors.fill: parent
        hoverEnabled: true
        cursorShape: Qt.PointingHandCursor
        onClicked: root.tile()
    }

    Connections {
        target: sd
        function onCardButtonClicked(button, card) {
            if (button === "left" || button === "right") root.half(card, button)
            else if (button === "focus") root.focusOn(card)
        }
    }

    // Shows a card as a normal card, then sizes and moves it. The size comes
    // first: a move keeps the whole card inside the area.
    function place(id, x, y, width, height) {
        sd.call("cards.collapse", {id: id}, function() {
            sd.call("cards.restore", {id: id}, function() {
                sd.call("cards.resize", {id: id, width: width, height: height}, function(g, err) {
                    if (err) { sd.log("resize: " + err.message); return }
                    sd.call("cards.move", {id: id, x: x, y: y})
                })
            })
        })
    }

    // The left or right half of the area cards may cover.
    function half(card, side) {
        sd.call("cards.geometry", {id: card.id}, function(g, err) {
            if (err) { sd.log("geometry: " + err.message); return }
            var a = g.area
            var w = Math.floor((a.width - gap) / 2)
            place(card.id, side === "left" ? a.x : a.x + w + gap, a.y, w, a.height)
        })
    }

    // Every other terminal goes to its icon; this one comes to the front.
    function focusOn(card) {
        sd.call("cards.list", {}, function(cards) {
            (cards || []).forEach(function(c) {
                if (c.kind === "terminal" && c.id !== card.id && !c.minimized) sd.call("cards.minimize", {id: c.id})
            })
            sd.call("cards.focus", {id: card.id})
        })
    }

    // The open terminals in one row of columns, as wide as there is room.
    function tile() {
        sd.call("cards.list", {}, function(cards, err) {
            var open = (cards || []).filter(function(c) { return c.kind === "terminal" && !c.minimized })
            if (open.length === 0) return
            sd.call("cards.geometry", {id: open[0].id}, function(g, err) {
                if (err) { sd.log("geometry: " + err.message); return }
                var a = g.area
                var w = Math.max(g.minWidth, Math.floor((a.width - gap * (open.length - 1)) / open.length))
                open.forEach(function(c, i) { place(c.id, a.x + i * (w + gap), a.y, w, a.height) })
            })
        })
    }
}
