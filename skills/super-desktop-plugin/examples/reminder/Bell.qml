import QtQuick
import QtQuick.Shapes

// A bell icon drawn in one color (24×24 design, scaled to the item's size).
Item {
    id: bell
    property color color: "white"  // set by the user of the icon. sd-lint: ignore theme-colors
    implicitWidth: 24; implicitHeight: 24

    Shape {
        width: 24; height: 24
        scale: Math.min(bell.width, bell.height) / 24
        anchors.centerIn: parent
        preferredRendererType: Shape.CurveRenderer
        ShapePath {
            strokeWidth: 0
            strokeColor: "transparent"
            fillColor: bell.color
            PathSvg { path: "M12 22c1.1 0 2-.9 2-2h-4c0 1.1.89 2 2 2zm6-6v-5c0-3.07-1.64-5.64-4.5-6.32V4c0-.83-.67-1.5-1.5-1.5s-1.5.67-1.5 1.5v.68C7.63 5.36 6 7.92 6 11v5l-2 2v1h16v-1l-2-2z" }
        }
    }
}
