// Desktop capture and drag pasteboards which GPUI's file-drop events do not expose.
#import <AppKit/AppKit.h>
#import <ApplicationServices/ApplicationServices.h>
#import <Carbon/Carbon.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <dlfcn.h>
#import <objc/runtime.h>

typedef void (*PixelReceiver)(void *, uint32_t, uint32_t, const uint8_t *);
int xrose_capture(uint32_t display, const float *region, void *context, PixelReceiver receive) {
    @autoreleasepool {
        if (!CGPreflightScreenCaptureAccess() && !CGRequestScreenCaptureAccess())
            return 1;
        CGImageRef frame = NULL;
        // Copy the fractions before handing work to asynchronous OS callbacks.
        CGRect unit = CGRectMake(region[0], region[1], region[2], region[3]);
        if (@available(macOS 14, *)) {
            dispatch_semaphore_t ready = dispatch_semaphore_create(0);
            // Blocks may finish after the worker times out. Retain the result in
            // an ARC-managed object and never use the caller's pointers in a block.
            __block id result = nil;
            [SCShareableContent
                getShareableContentExcludingDesktopWindows:NO
                                       onScreenWindowsOnly:YES
                                         completionHandler:^(SCShareableContent *content,
                                                             NSError *__attribute__((unused)) error) {
                                           for (SCDisplay *screen in content.displays) {
                                               if (screen.displayID != display)
                                                   continue;
                                               NSMutableArray *excluded = [NSMutableArray array];
                                               for (SCRunningApplication *app in content.applications)
                                                   if (app.processID ==
                                                       NSProcessInfo.processInfo.processIdentifier)
                                                       [excluded addObject:app];
                                               SCContentFilter *filter =
                                                   [[SCContentFilter alloc] initWithDisplay:screen
                                                                      excludingApplications:excluded
                                                                           exceptingWindows:@[]];
                                               SCStreamConfiguration *cfg = [SCStreamConfiguration new];
                                               CGRect area =
                                                   CGRectMake(unit.origin.x * screen.width,
                                                              unit.origin.y * screen.height,
                                                              unit.size.width * screen.width,
                                                              unit.size.height * screen.height);
                                               cfg.sourceRect = area;
                                               CGFloat scale =
                                                   fmin(filter.pointPixelScale,
                                                        1024.0 / MAX(area.size.width, area.size.height));
                                               cfg.width = MAX(1, round(area.size.width * scale));
                                               cfg.height = MAX(1, round(area.size.height * scale));
                                               cfg.showsCursor = NO;
                                               [SCScreenshotManager
                                                   captureImageWithFilter:filter
                                                            configuration:cfg
                                                        completionHandler:^(
                                                            CGImageRef image,
                                                            NSError *__attribute__((unused)) error) {
                                                          if (image)
                                                              result = (__bridge id)image;
                                                          dispatch_semaphore_signal(ready);
                                                        }];
                                               return;
                                           }
                                           dispatch_semaphore_signal(ready);
                                         }];
            if (dispatch_semaphore_wait(ready, dispatch_time(DISPATCH_TIME_NOW, 12 * NSEC_PER_SEC)))
                return 2;
            if (result)
                frame = CGImageRetain((__bridge CGImageRef)result);
        } else {
            CGImageRef (*legacyCapture)(CGDirectDisplayID) = dlsym(RTLD_DEFAULT, "CGDisplayCreateImage");
            if (legacyCapture) {
                CGImageRef desktop = legacyCapture(display);
                if (desktop) {
                    size_t w = CGImageGetWidth(desktop), h = CGImageGetHeight(desktop);
                    frame = CGImageCreateWithImageInRect(
                        desktop, CGRectMake(w * unit.origin.x, h * unit.origin.y, w * unit.size.width,
                                            h * unit.size.height));
                    CGImageRelease(desktop);
                }
            }
        }
        if (!frame)
            return 2;
        size_t w = CGImageGetWidth(frame), h = CGImageGetHeight(frame);
        if (!w || !h || w > 36000000 / h) {
            CGImageRelease(frame);
            return 2;
        }
        CGColorSpaceRef space = CGColorSpaceCreateDeviceRGB();
        CGContextRef bitmap = CGBitmapContextCreate(
            NULL, w, h, 8, w * 4, space, kCGImageAlphaPremultipliedLast | kCGBitmapByteOrder32Big);
        CGColorSpaceRelease(space);
        if (bitmap) {
            CGContextDrawImage(bitmap, CGRectMake(0, 0, w, h), frame);
            receive(context, (uint32_t)w, (uint32_t)h, CGBitmapContextGetData(bitmap));
            CGContextRelease(bitmap);
        }
        CGImageRelease(frame);
        return bitmap ? 0 : 2;
    }
}

// Nonactivating panels leave ordinary scrolling and application focus with the desktop.
enum { FINDER_CLICK = 1u, FINDER_CONFIRM_KEY = 8u };
typedef CGError (*SetConnectionProperty)(int, int, CFStringRef, CFTypeRef);
@interface XroseFinderInput : NSObject {
  @public
    NSRect screenFrame;
    CGPoint clickPoint;
    unsigned int actions, revision;
    BOOL hit, leftDown, rightDown;
    NSTimeInterval passWheelUntil;
    uint32_t scrollCount;
    id globalMonitor, localMonitor, activation, space;
    float zoomDelta;
    __weak NSWindow *panel;
    NSTimeInterval zoomTailUntil;
    EventHotKeyRef keys[3];
    EventHandlerRef handler;
    CGDirectDisplayID displayID;
    BOOL cursorHidden, running;
    SetConnectionProperty setConnectionProperty;
    int cursorConnection;
    id previousCursorPolicy;
    CFMachPortRef tap;
    CFRunLoopSourceRef tapSource;
}
@end
@implementation XroseFinderInput
@end

// A passive window cannot own cursor rectangles. On systems that expose this
// WindowServer connection option, allow only this process to affect the cursor
// while another application has focus. Restore the option with the session.
static void allowBackgroundCursor(XroseFinderInput *s) {
    int (*connectionID)(void) = dlsym(RTLD_DEFAULT, "CGSMainConnectionID");
    CGError (*copyProperty)(int, int, CFStringRef, CFTypeRef *) =
        dlsym(RTLD_DEFAULT, "CGSCopyConnectionProperty");
    SetConnectionProperty setProperty = dlsym(RTLD_DEFAULT, "CGSSetConnectionProperty");
    if (!connectionID || !copyProperty || !setProperty)
        return;
    int connection = connectionID();
    CFTypeRef previous = NULL;
    copyProperty(connection, connection, CFSTR("SetsCursorInBackground"), &previous);
    s->previousCursorPolicy = CFBridgingRelease(previous);
    if (setProperty(connection, connection, CFSTR("SetsCursorInBackground"), kCFBooleanTrue) == kCGErrorSuccess) {
        s->cursorConnection = connection;
        s->setConnectionProperty = setProperty;
    }
}

static void setFinderCursorVisible(XroseFinderInput *s, BOOL visible) {
    if (visible && s->cursorHidden) {
        if (CGDisplayShowCursor(s->displayID) == kCGErrorSuccess)
            s->cursorHidden = NO;
    } else if (!visible && !s->cursorHidden) {
        s->cursorHidden = CGDisplayHideCursor(s->displayID) == kCGErrorSuccess;
    }
}
// AppKit, Carbon and the session tap all dispatch on the main thread.
static NSMutableArray<XroseFinderInput *> *activeFinders;
static XroseFinderInput *cursorPolicyOwner;

// Global monitors can observe another application's click but cannot consume it.
static void updateMouseTarget(XroseFinderInput *s, NSEventModifierFlags flags) {
    if (s->tap)
        return;
    NSTimeInterval now = NSProcessInfo.processInfo.systemUptime;
    BOOL intercept = s->leftDown || s->rightDown || (flags & NSEventModifierFlagCommand) ||
                     now < s->zoomTailUntil || (s->hit && now >= s->passWheelUntil);
    NSWindow *panel = s->panel;
    if (panel.ignoresMouseEvents == !intercept)
        return;
    // Fully clear pixels are not hit-tested by WindowServer.
    panel.backgroundColor = intercept ? [NSColor colorWithWhite:0 alpha:1.0 / 255.0] : NSColor.clearColor;
    panel.ignoresMouseEvents = !intercept;
}

void xrose_finder_set_hit(void *ptr, bool hit) {
    XroseFinderInput *s = (__bridge XroseFinderInput *)ptr;
    s->hit = hit;
    updateMouseTarget(s, NSEvent.modifierFlags);
}

static CGPoint finderPosition(XroseFinderInput *s) {
    NSPoint p = NSEvent.mouseLocation;
    return CGPointMake(p.x - NSMinX(s->screenFrame), NSMaxY(s->screenFrame) - p.y);
}
static OSStatus finderKey(EventHandlerCallRef next, EventRef event, void *context) {
    (void)next;
    (void)context;
    EventHotKeyID key;
    if (GetEventParameter(event, kEventParamDirectObject, typeEventHotKeyID, NULL, sizeof(key), NULL,
                          &key) != noErr || key.signature != 'XrQR')
        return eventNotHandledErr;
    for (XroseFinderInput *s in activeFinders) {
        if (key.id == 0) {
            s->actions |= 2;
        } else if (NSMouseInRect(NSEvent.mouseLocation, s->screenFrame, NO)) {
            s->clickPoint = finderPosition(s);
            s->actions = (s->actions & ~FINDER_CLICK) | FINDER_CONFIRM_KEY;
        }
    }
    return noErr;
}
static BOOL consumeZoom(XroseFinderInput *s, NSEvent *event) {
    BOOL command = (event.modifierFlags & NSEventModifierFlagCommand) != 0;
    BOOL trackpadGesture = event.phase != NSEventPhaseNone || event.momentumPhase != NSEventPhaseNone;
    BOOL tail = trackpadGesture && event.phase != NSEventPhaseBegan &&
                event.timestamp < s->zoomTailUntil;
    if (!command && !tail) {
        s->zoomTailUntil = 0;
        return NO;
    }
    if (command)
        s->zoomDelta += event.scrollingDeltaY / (event.hasPreciseScrollingDeltas ? 20. : 1.);
    // Keep the end and momentum of this gesture away from the underlying page
    // even when Command is released before the trackpad comes to rest.
    s->zoomTailUntil = trackpadGesture && !(event.momentumPhase & NSEventPhaseEnded) &&
                       !(event.phase & NSEventPhaseCancelled) ? event.timestamp + 0.12 : 0;
    return YES;
}
static CGPoint finderEventPoint(XroseFinderInput *s, CGEventRef event) {
    CGPoint point = CGEventGetLocation(event);
    CGRect display = CGDisplayBounds(s->displayID);
    return CGPointMake(point.x - display.origin.x, point.y - display.origin.y);
}
// Only the owner of a press may confirm on release, including across displays.
static BOOL handleButton(XroseFinderInput *s, CGEventType type, CGPoint p) {
    switch (type) {
    case kCGEventLeftMouseDown:
        s->leftDown = YES;
        s->clickPoint = p;
        return YES;
    case kCGEventRightMouseDown:
        s->rightDown = YES;
        return YES;
    case kCGEventLeftMouseUp:
        if (!s->leftDown)
            return NO;
        s->leftDown = NO;
        s->actions = (s->actions & ~FINDER_CONFIRM_KEY) | FINDER_CLICK;
        return YES;
    case kCGEventRightMouseUp:
        if (!s->rightDown)
            return NO;
        s->rightDown = NO;
        s->actions |= 2;
        return YES;
    default:
        return NO;
    }
}
static CGEventRef captureButtons(CGEventTapProxy proxy, CGEventType type, CGEventRef event, void *ptr) {
    (void)proxy;
    XroseFinderInput *s = (__bridge XroseFinderInput *)ptr;
    if (type == kCGEventTapDisabledByTimeout || type == kCGEventTapDisabledByUserInput) {
        CGEventTapEnable(s->tap, true);
        return event;
    }
    CGPoint p = finderEventPoint(s, event);
    if (type != kCGEventLeftMouseUp && type != kCGEventRightMouseUp &&
        !CGRectContainsPoint(CGRectMake(0, 0, s->screenFrame.size.width, s->screenFrame.size.height), p))
        return event;
    if (type == kCGEventScrollWheel)
        return consumeZoom(s, [NSEvent eventWithCGEvent:event]) ? NULL : event;
    return handleButton(s, type, p) ? NULL : event;
}
void *xrose_begin_finder(void *view, uint32_t display) {
    NSWindow *window = [(__bridge NSView *)view window];
    XroseFinderInput *s = [XroseFinderInput new];
    for (NSScreen *screen in NSScreen.screens) {
        if ([screen.deviceDescription[@"NSScreenNumber"] unsignedIntValue] != display)
            continue;
        s->screenFrame = screen.frame;
        break;
    }
    if (!window || NSIsEmptyRect(s->screenFrame))
        return NULL;
    s->scrollCount =
        CGEventSourceCounterForEventType(kCGEventSourceStateCombinedSessionState, kCGEventScrollWheel);
    s->displayID = display;
    s->panel = window;
    s->running = YES;
    // Set the hit policy before ordering the window; it never becomes key.
    window.ignoresMouseEvents = YES;
    window.opaque = NO;
    window.hidesOnDeactivate = NO;
    window.canHide = NO;
    window.backgroundColor = NSColor.clearColor;
    window.hasShadow = NO;
    dispatch_async(dispatch_get_main_queue(), ^{
      if (!s->running)
          return;
      window.styleMask = NSWindowStyleMaskBorderless | NSWindowStyleMaskNonactivatingPanel;
      window.collectionBehavior = NSWindowCollectionBehaviorCanJoinAllSpaces |
                                  NSWindowCollectionBehaviorFullScreenAuxiliary |
                                  NSWindowCollectionBehaviorStationary;
      if (@available(macOS 13, *))
          window.collectionBehavior |= NSWindowCollectionBehaviorCanJoinAllApplications;
      window.level = NSFloatingWindowLevel;
      [window setFrame:s->screenFrame display:YES];
      [window orderFrontRegardless];
      updateMouseTarget(s, NSEvent.modifierFlags);
      // The button entry orders out and hides the main app on this queue.
      // Hiding the cursor before that transition lets AppKit reveal it again.
      if (s == cursorPolicyOwner)
          setFinderCursorVisible(s, NO);
    });
    // Use an existing system-wide tap permission when available; otherwise
    // WindowServer routes selectable clicks directly to the nonactivating panel.
    if (AXIsProcessTrusted()) {
        CGEventMask buttons = CGEventMaskBit(kCGEventLeftMouseDown) |
                              CGEventMaskBit(kCGEventRightMouseDown) |
                              CGEventMaskBit(kCGEventLeftMouseUp) | CGEventMaskBit(kCGEventRightMouseUp) |
                              CGEventMaskBit(kCGEventScrollWheel);
        s->tap = CGEventTapCreate(kCGSessionEventTap, kCGHeadInsertEventTap, kCGEventTapOptionDefault,
                                  buttons, captureButtons, (__bridge void *)s);
        if (s->tap) {
            s->tapSource = CFMachPortCreateRunLoopSource(NULL, s->tap, 0);
            CFRunLoopAddSource(CFRunLoopGetMain(), s->tapSource, kCFRunLoopCommonModes);
        }
    }
    NSEventMask observed = NSEventMaskFlagsChanged | NSEventMaskScrollWheel;
    void (^observe)(NSEvent *) = ^(NSEvent *e) {
      if (e.type == NSEventTypeScrollWheel && NSMouseInRect(NSEvent.mouseLocation, s->screenFrame, NO)) {
          s->revision++;
          if (!(e.modifierFlags & NSEventModifierFlagCommand))
              s->passWheelUntil = e.timestamp + 0.18;
      }
      updateMouseTarget(s, e.modifierFlags);
    };
    s->globalMonitor = [NSEvent addGlobalMonitorForEventsMatchingMask:observed | NSEventMaskRightMouseUp
        handler:^(NSEvent *e) {
          // A passive panel can still dismiss on right-click outside a target.
          if (e.type == NSEventTypeRightMouseUp) {
              if (!s->tap && NSMouseInRect(NSEvent.mouseLocation, s->screenFrame, NO))
                  s->actions |= 2;
          } else {
              observe(e);
          }
        }];
    NSEventMask buttons = NSEventMaskLeftMouseDown | NSEventMaskLeftMouseUp |
                          NSEventMaskRightMouseDown | NSEventMaskRightMouseUp;
    s->localMonitor = [NSEvent addLocalMonitorForEventsMatchingMask:observed | buttons | NSEventMaskKeyDown
        handler:^NSEvent *(NSEvent *e) {
          if (!s->tap && e.window == s->panel) {
              if (e.type == NSEventTypeScrollWheel && consumeZoom(s, e))
                  return nil;
              if ((NSEventMaskFromType(e.type) & buttons) && e.CGEvent &&
                  handleButton(s, CGEventGetType(e.CGEvent), finderEventPoint(s, e.CGEvent)))
                  return nil;
          }
          if (e.type == NSEventTypeKeyDown && e.keyCode == kVK_Escape) {
              s->actions |= 2;
              return nil;
          }
          if (NSEventMaskFromType(e.type) & observed)
              observe(e);
          return e;
        }];
    NSNotificationCenter *center = NSWorkspace.sharedWorkspace.notificationCenter;
    void (^changed)(NSNotification *) = ^(NSNotification *n) {
      s->revision++;
      if (s == cursorPolicyOwner && s->cursorHidden &&
          [n.name isEqual:NSWorkspaceDidActivateApplicationNotification]) {
          setFinderCursorVisible(s, YES);
          setFinderCursorVisible(s, NO);
      }
    };
    s->activation = [center addObserverForName:NSWorkspaceDidActivateApplicationNotification
                                        object:nil
                                         queue:NSOperationQueue.mainQueue
                                    usingBlock:changed];
    s->space = [center addObserverForName:NSWorkspaceActiveSpaceDidChangeNotification
                                   object:nil
                                    queue:NSOperationQueue.mainQueue
                               usingBlock:changed];
    if (!activeFinders)
        activeFinders = [NSMutableArray array];
    if (!activeFinders.count) {
        cursorPolicyOwner = s;
        allowBackgroundCursor(s);
        EventTypeSpec type = {kEventClassKeyboard, kEventHotKeyPressed};
        if (InstallEventHandler(GetApplicationEventTarget(), finderKey, 1, &type, NULL,
                                &s->handler) == noErr) {
            const UInt32 codes[] = {kVK_Escape, kVK_Return, kVK_Space};
            for (UInt32 i = 0; i < 3; i++) {
                EventHotKeyID key = {'XrQR', i};
                RegisterEventHotKey(codes[i], 0, key, GetApplicationEventTarget(), 0, &s->keys[i]);
            }
        }
    }
    [activeFinders addObject:s];
    return (__bridge_retained void *)s;
}
unsigned int xrose_finder_input(void *ptr, float *x, float *y, float *clickX, float *clickY,
                                unsigned int *revision, float *zoom) {
    XroseFinderInput *s = (__bridge XroseFinderInput *)ptr;
    // Flags-changed monitors can be restricted when another app has focus.
    updateMouseTarget(s, NSEvent.modifierFlags);
    uint32_t scroll =
        CGEventSourceCounterForEventType(kCGEventSourceStateCombinedSessionState, kCGEventScrollWheel);
    if (CGEventSourceKeyState(kCGEventSourceStateCombinedSessionState, kVK_Escape))
        s->actions |= 2;
    if (scroll != s->scrollCount)
        s->revision++;
    s->scrollCount = scroll;
    CGPoint p = finderPosition(s);
    *x = p.x;
    *y = p.y;
    *clickX = s->clickPoint.x;
    *clickY = s->clickPoint.y;
    *revision = s->revision;
    *zoom = s->zoomDelta;
    s->zoomDelta = 0;
    unsigned int result = s->actions;
    if (s->leftDown)
        result |= 4;
    s->actions = 0;
    return result;
}
void xrose_end_finder(void *ptr) {
    XroseFinderInput *s = (__bridge_transfer XroseFinderInput *)ptr;
    s->running = NO;
    if (s->tap) {
        CFMachPortInvalidate(s->tap);
        CFRunLoopRemoveSource(CFRunLoopGetMain(), s->tapSource, kCFRunLoopCommonModes);
        CFRelease(s->tapSource);
        CFRelease(s->tap);
    }
    if (s->globalMonitor)
        [NSEvent removeMonitor:s->globalMonitor];
    if (s->localMonitor)
        [NSEvent removeMonitor:s->localMonitor];
    s->globalMonitor = nil;
    s->localMonitor = nil;
    NSNotificationCenter *center = NSWorkspace.sharedWorkspace.notificationCenter;
    [center removeObserver:s->activation];
    [center removeObserver:s->space];
    s->activation = nil;
    s->space = nil;
    [activeFinders removeObject:s];
    if (!activeFinders.count) {
        XroseFinderInput *owner = cursorPolicyOwner;
        setFinderCursorVisible(owner, YES);
        for (size_t i = 0; i < 3; i++)
            if (owner->keys[i])
                UnregisterEventHotKey(owner->keys[i]);
        if (owner->handler)
            RemoveEventHandler(owner->handler);
        if (owner->setConnectionProperty)
            owner->setConnectionProperty(owner->cursorConnection, owner->cursorConnection,
                                         CFSTR("SetsCursorInBackground"),
                                         (__bridge CFTypeRef)(owner->previousCursorPolicy ?: @NO));
        cursorPolicyOwner = nil;
    }
}

bool xrose_play_import_sound(bool success) {
    NSSound *sound = [NSSound soundNamed:(success ? @"Hero" : @"Basso")];
    return [sound play];
}

// Window ordering sends synchronous AppKit notifications. Schedule it outside
// GPUI's current window update to allow native activation callbacks to run.
void xrose_main_visible(void *view, bool show) {
    NSWindow *window = [(__bridge NSView *)view window];
    dispatch_async(dispatch_get_main_queue(), ^{
      if (show) {
          [NSApp unhideWithoutActivation];
          if (@available(macOS 14, *))
              [NSApp activate];
          else
              [NSApp activateIgnoringOtherApps:YES];
          [window makeKeyAndOrderFront:nil];
      } else {
          [window orderOut:nil];
          // Yield the active Stage Manager set to the application behind the main window.
          [NSApp hide:nil];
      }
    });
}

// Some browsers advertise both an image and its source URL. Prefer the pixels
// for QR import; an actual file drag/paste is still handled as a file.
static NSArray<NSString *> *imageTypes(void) {
    return @[ NSPasteboardTypePNG, NSPasteboardTypeTIFF, @"public.jpeg", @"org.webmproject.webp" ];
}
int xrose_paste_image(size_t limit, void *context, void (*receive)(void *, const void *, size_t)) {
    NSPasteboard *board = NSPasteboard.generalPasteboard;
    if ([board.types containsObject:NSPasteboardTypeFileURL])
        return 0;
    NSString *type = [board availableTypeFromArray:imageTypes()];
    if (!type)
        return 0;
    NSData *data = [board dataForType:type];
    if (!data.length)
        return 0;
    if (data.length > limit)
        return -1;
    receive(context, data.bytes, data.length);
    return 1;
}

// Subclass just this window; leave the platform class, other windows, and GPUI's
// file-drag lifecycle intact. Only inspect drag data on an actual drop.
extern bool xrose_accepts_drop(void);
extern bool xrose_receive_drop(bool image, const void *data, size_t length);
static Class original;
static NSString *dropType(id<NSDraggingInfo> drag) {
    if (!xrose_accepts_drop() || drag.draggingSource != nil)
        return nil;
    NSPasteboard *board = drag.draggingPasteboard;
    if ([board.types containsObject:NSPasteboardTypeFileURL])
        return nil;
    return [board availableTypeFromArray:[imageTypes() arrayByAddingObjectsFromArray:@[
                      NSPasteboardTypeURL, NSPasteboardTypeString
                  ]]];
}
static NSDragOperation entered(id window, SEL cmd, id<NSDraggingInfo> drag) {
    if (dropType(drag))
        return NSDragOperationCopy;
    return ((NSDragOperation (*)(id, SEL, id))class_getMethodImplementation(original, cmd))(window, cmd,
                                                                                            drag);
}
static BOOL perform(id window, SEL cmd, id<NSDraggingInfo> drag) {
    NSString *type = dropType(drag);
    if (!type)
        return ((BOOL (*)(id, SEL, id))class_getMethodImplementation(original, cmd))(window, cmd, drag);
    BOOL image = ![type isEqual:NSPasteboardTypeURL] && ![type isEqual:NSPasteboardTypeString];
    NSData *data =
        image ? [drag.draggingPasteboard dataForType:type]
              : [[drag.draggingPasteboard stringForType:type] dataUsingEncoding:NSUTF8StringEncoding];
    return data && xrose_receive_drop(image, data.bytes, data.length);
}
void xrose_install_drop(void *view) {
    NSWindow *window = [(__bridge NSView *)view window];
    if (!window)
        return;
    static Class subclass;
    if (!subclass) {
        original = object_getClass(window);
        subclass = objc_allocateClassPair(original, "XroseImportWindow", 0);
        if (!subclass)
            return;
        SEL selectors[] = {@selector(draggingEntered:), @selector(draggingUpdated:),
                           @selector(performDragOperation:)};
        IMP methods[] = {(IMP)entered, (IMP)entered, (IMP)perform};
        for (int i = 0; i < 3; i++)
            class_addMethod(subclass, selectors[i], methods[i],
                            method_getTypeEncoding(class_getInstanceMethod(original, selectors[i])));
        objc_registerClassPair(subclass);
    }
    object_setClass(window, subclass);
    [window registerForDraggedTypes:@[
        @"NSFilenamesPboardType", NSPasteboardTypePNG, NSPasteboardTypeTIFF, @"public.jpeg",
        @"org.webmproject.webp", NSPasteboardTypeURL, NSPasteboardTypeString
    ]];
}
