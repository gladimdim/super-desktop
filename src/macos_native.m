#import <AppKit/AppKit.h>
#import <Carbon/Carbon.h>

// AppKit and Carbon are called only on GTK's main thread.
static NSRunningApplication *previousApplication;
static id screenObserver;
static EventHotKeyRef shortcut;
static EventHandlerRef shortcutHandler;
static void (*toggleCallback)(void);
static void (*quitCallback)(void);
static BOOL capturingShortcut;
static BOOL hasShortcutConfiguration;
static unsigned int shortcutKey, shortcutModifiers;

@interface SDApplicationActions : NSObject
- (void)toggle:(id)sender;
- (void)quit:(id)sender;
- (void)reopen:(NSAppleEventDescriptor *)event reply:(NSAppleEventDescriptor *)reply;
@end
@implementation SDApplicationActions
- (void)toggle:(id)sender { if (toggleCallback) toggleCallback(); }
- (void)quit:(id)sender { if (quitCallback) quitCallback(); }
- (void)reopen:(NSAppleEventDescriptor *)event reply:(NSAppleEventDescriptor *)reply {
    if (toggleCallback) toggleCallback();
}
@end
static SDApplicationActions *applicationActions;
static NSStatusItem *statusItem;
static NSMenuItem *shortcutStatus;

void sd_macos_configure(void *pointer, bool opaque) {
    NSWindow *window = (__bridge NSWindow *)pointer;
    window.level = NSFloatingWindowLevel;
    window.collectionBehavior = NSWindowCollectionBehaviorCanJoinAllSpaces |
                                NSWindowCollectionBehaviorFullScreenAuxiliary;
    // The main workspace paints an opaque, themed GTK root. Keep AppKit in
    // agreement so disappearing child surfaces cannot expose stale alpha.
    // Picker/pairing windows retain their transparent composition.
    window.opaque = opaque;
    window.backgroundColor = opaque ? NSColor.windowBackgroundColor : NSColor.clearColor;
    window.hasShadow = NO;
    if (screenObserver) [[NSNotificationCenter defaultCenter] removeObserver:screenObserver];
    __weak NSWindow *weakWindow = window;
    screenObserver = [[NSNotificationCenter defaultCenter]
        addObserverForName:NSApplicationDidChangeScreenParametersNotification
        object:nil queue:nil usingBlock:^(NSNotification *notification) {
            NSWindow *live = weakWindow;
            NSScreen *screen = live.screen ?: NSScreen.mainScreen;
            if (live && screen) [live setFrame:screen.visibleFrame display:YES];
        }];
}

static NSScreen *presentationScreen(NSWindow *window) {
    NSScreen *screen = window.screen ?: NSScreen.mainScreen;
    NSPoint mouse = NSEvent.mouseLocation;
    for (NSScreen *candidate in NSScreen.screens)
        if (NSPointInRect(mouse, candidate.frame)) return candidate;
    return screen;
}

void sd_macos_bounds(void *pointer, int *width, int *height) {
    NSScreen *screen = presentationScreen((__bridge NSWindow *)pointer);
    if (screen) {
        *width = (int)screen.visibleFrame.size.width;
        *height = (int)screen.visibleFrame.size.height;
    }
}

void sd_macos_present(void *pointer) {
    NSWindow *window = (__bridge NSWindow *)pointer;
    NSRunningApplication *front = NSWorkspace.sharedWorkspace.frontmostApplication;
    if (front.processIdentifier != NSProcessInfo.processInfo.processIdentifier)
        previousApplication = front;
    NSScreen *screen = presentationScreen(window);
    if (screen) [window setFrame:screen.visibleFrame display:YES];
    window.ignoresMouseEvents = NO;
    [NSApp activateIgnoringOtherApps:YES];
    [window makeKeyAndOrderFront:nil];
}

void sd_macos_input(void *pointer, bool enabled) {
    NSWindow *window = (__bridge NSWindow *)pointer;
    window.ignoresMouseEvents = !enabled;
    if (!enabled && NSApp.active && previousApplication && !previousApplication.terminated) {
        [previousApplication activateWithOptions:0];
        previousApplication = nil;
    }
}

static OSStatus hotkeyEvent(EventHandlerCallRef next, EventRef event, void *data) {
    EventHotKeyID key;
    if (GetEventParameter(event, kEventParamDirectObject, typeEventHotKeyID,
                          NULL, sizeof(key), NULL, &key) != noErr || key.signature != 'SDsk')
        return eventNotHandledErr;
    if (!capturingShortcut && toggleCallback) toggleCallback();
    return noErr;
}

void sd_macos_setup(void (*toggle)(void), void (*quit)(void)) {
    toggleCallback = toggle;
    quitCallback = quit;
    if (!applicationActions) {
        applicationActions = [SDApplicationActions new];
        [[NSAppleEventManager sharedAppleEventManager] setEventHandler:applicationActions
            andSelector:@selector(reopen:reply:) forEventClass:kCoreEventClass andEventID:kAEReopenApplication];
        NSMenu *menu = [NSMenu new];
        NSMenuItem *application = [NSMenuItem new];
        [menu addItem:application];
        NSMenu *items = [[NSMenu alloc] initWithTitle:@"SUPER DESKTOP"];
        NSMenuItem *toggle = [[NSMenuItem alloc] initWithTitle:@"Show / Hide SUPER DESKTOP"
            action:@selector(toggle:) keyEquivalent:@""];
        toggle.target = applicationActions;
        [items addItem:toggle];
        [items addItem:NSMenuItem.separatorItem];
        NSMenuItem *quit = [items addItemWithTitle:@"Quit SUPER DESKTOP" action:@selector(quit:) keyEquivalent:@"q"];
        quit.target = applicationActions;
        application.submenu = items;
        NSApp.mainMenu = menu;

        statusItem = [NSStatusBar.systemStatusBar statusItemWithLength:NSVariableStatusItemLength];
        statusItem.button.title = @"SD";
        statusItem.button.toolTip = @"SUPER DESKTOP — Show / Hide";
        statusItem.menu = [items copy];
        shortcutStatus = [[NSMenuItem alloc] initWithTitle:@"Shortcut: starting…"
            action:nil keyEquivalent:@""];
        [statusItem.menu insertItem:shortcutStatus atIndex:1];
    }
}

void sd_macos_shortcut_status(const char *message) {
    shortcutStatus.title = [NSString stringWithUTF8String:message] ?: @"Shortcut unavailable";
}

int sd_macos_hotkey(unsigned int key, unsigned int modifiers) {
    if (!shortcutHandler) {
        EventTypeSpec type = { kEventClassKeyboard, kEventHotKeyReleased };
        OSStatus result = InstallApplicationEventHandler(hotkeyEvent, 1, &type, NULL, &shortcutHandler);
        if (result != noErr) return result;
    }
    EventHotKeyRef replacement = NULL;
    EventHotKeyID identifier = { 'SDsk', 1 };
    OSStatus result = RegisterEventHotKey(key, modifiers, identifier,
                                         GetApplicationEventTarget(), 0, &replacement);
    if (result != noErr) return result; // Keep the old working shortcut.
    if (shortcut) UnregisterEventHotKey(shortcut);
    shortcut = replacement;
    shortcutKey = key;
    shortcutModifiers = modifiers;
    hasShortcutConfiguration = YES;
    return noErr;
}

int sd_macos_capture_shortcut(bool capture) {
    if (capturingShortcut == capture) return noErr;
    capturingShortcut = capture;
    if (capture && shortcut) {
        UnregisterEventHotKey(shortcut);
        shortcut = NULL;
    } else if (!capture && hasShortcutConfiguration) {
        int result = sd_macos_hotkey(shortcutKey, shortcutModifiers);
        if (result != noErr) NSLog(@"SUPER DESKTOP: restoring shortcut failed (%d)", result);
        return result;
    }
    return noErr;
}
