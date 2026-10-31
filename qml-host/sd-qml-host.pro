TEMPLATE = app
TARGET = sd-qml-host
QT += core gui qml quick
CONFIG += c++17 release
CONFIG -= app_bundle
SOURCES += main.cpp
LIBS += -lutil
RESOURCES += qml.qrc
