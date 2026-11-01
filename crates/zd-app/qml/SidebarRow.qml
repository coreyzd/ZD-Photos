import QtQuick
import QtQuick.Controls

// One row in the sidebar: icon glyph, name, photo count. Offline drives are dimmed.
// Rows are 40 px tall so they're comfortable to tap on her touchscreen.
Rectangle {
    id: row
    property string glyph: ""
    property string label: ""
    property string detail: ""
    property bool online: true
    property bool selected: false
    signal clicked()
    signal rightClicked()
    signal doubleClicked()

    width: parent ? parent.width : 240
    height: 40
    radius: 20
    color: selected ? "#28405e" : hover.hovered ? "#262626" : "transparent"

    Text {
        id: icon
        anchors.left: parent.left
        anchors.leftMargin: 20
        anchors.verticalCenter: parent.verticalCenter
        width: 20
        text: row.glyph
        color: row.selected ? "#a8c7fa" : "#c4c4c4"
        font.pixelSize: 16
        horizontalAlignment: Text.AlignHCenter
        opacity: row.online ? 1 : 0.45
    }
    Text {
        anchors.left: icon.right
        anchors.leftMargin: 12
        anchors.right: count.left
        anchors.rightMargin: 8
        anchors.verticalCenter: parent.verticalCenter
        text: row.label + (row.online ? "" : "  (offline)")
        color: row.selected ? "#a8c7fa" : "#e3e3e3"
        font.pixelSize: 14
        elide: Text.ElideRight
        opacity: row.online ? 1 : 0.55
    }
    Text {
        id: count
        anchors.right: parent.right
        anchors.rightMargin: 16
        anchors.verticalCenter: parent.verticalCenter
        text: row.detail
        color: "#a8a8a8"
        font.pixelSize: 12
    }

    HoverHandler { id: hover }
    TapHandler {
        acceptedButtons: Qt.LeftButton
        onTapped: row.clicked()
        onDoubleTapped: row.doubleClicked()
        // Long-press on the touchscreen opens the same menu as right-click.
        onLongPressed: row.rightClicked()
    }
    TapHandler {
        acceptedButtons: Qt.RightButton
        onTapped: row.rightClicked()
    }
}
