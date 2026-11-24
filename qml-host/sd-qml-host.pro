TEMPLATE = app
TARGET = sd-qml-host
QT += core gui qml quick
CONFIG += c++17 release
CONFIG -= app_bundle
QMAKE_CXXFLAGS += -D_FORTIFY_SOURCE=2
QMAKE_LFLAGS += -Wl,-z,relro,-z,now
SOURCES += main.cpp
LIBS += -lutil
RESOURCES += qml.qrc
