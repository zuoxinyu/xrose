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

// A mouse-transparent panel leaves wheel gestures and application switching with
// the desktop. Only mouse-downs, scroll invalidation and Escape are observed.
typedef CGError (*SetConnectionProperty)(int, int, CFStringRef, CFTypeRef);
@interface XroseFinderInput : NSObject {
  @public
    NSRect screenFrame;
    CGPoint clickPoint;
    unsigned int actions, revision;
    uint32_t leftCount, rightCount, scrollCount;
    id globalMonitor, localMonitor, activation, space;
    EventHotKeyRef escape;
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

void xrose_finder_cursor(void *ptr, bool visible) {
    XroseFinderInput *s = (__bridge XroseFinderInput *)ptr;
    if (visible && s->cursorHidden) {
        if (CGDisplayShowCursor(s->displayID) == kCGErrorSuccess)
            s->cursorHidden = NO;
    } else if (!visible && !s->cursorHidden) {
        s->cursorHidden = CGDisplayHideCursor(s->displayID) == kCGErrorSuccess;
    }
}
static OSStatus finderEscape(EventHandlerCallRef next, EventRef event, void *context) {
    (void)next;
    EventHotKeyID key;
    if (GetEventParameter(event, kEventParamDirectObject, typeEventHotKeyID, NULL, sizeof(key), NULL,
                          &key) != noErr ||
        key.signature != 'XrQR')
        return eventNotHandledErr;
    ((__bridge XroseFinderInput *)context)->actions |= 2;
    return noErr;
}
static CGPoint finderPosition(XroseFinderInput *s) {
    NSPoint p = NSEvent.mouseLocation;
    return CGPointMake(p.x - NSMinX(s->screenFrame), NSMaxY(s->screenFrame) - p.y);
}
static CGPoint finderEventPoint(XroseFinderInput *s, CGEventRef event) {
    CGPoint point = CGEventGetLocation(event);
    CGRect display = CGDisplayBounds(s->displayID);
    return CGPointMake(point.x - display.origin.x, point.y - display.origin.y);
}
static CGEventRef captureButtons(CGEventTapProxy proxy, CGEventType type, CGEventRef event, void *ptr) {
    (void)proxy;
    XroseFinderInput *s = (__bridge XroseFinderInput *)ptr;
    if (type == kCGEventTapDisabledByTimeout || type == kCGEventTapDisabledByUserInput) {
        CGEventTapEnable(s->tap, true);
        return event;
    }
    CGPoint p = finderEventPoint(s, event);
    if (!CGRectContainsPoint(CGRectMake(0, 0, s->screenFrame.size.width, s->screenFrame.size.height), p))
        return event;
    if (type == kCGEventLeftMouseDown) {
        s->clickPoint = p;
        s->actions |= 1;
    }
    if (type == kCGEventRightMouseDown) {
        s->actions |= 2;
    }
    return NULL;
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
    s->leftCount =
        CGEventSourceCounterForEventType(kCGEventSourceStateCombinedSessionState, kCGEventLeftMouseDown);
    s->rightCount = CGEventSourceCounterForEventType(kCGEventSourceStateCombinedSessionState,
                                                     kCGEventRightMouseDown);
    s->scrollCount =
        CGEventSourceCounterForEventType(kCGEventSourceStateCombinedSessionState, kCGEventScrollWheel);
    s->displayID = display;
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
    });
    NSEventMask mask = NSEventMaskScrollWheel;
    // When input interception is already allowed, consume scanner clicks. No
    // accessibility prompt is needed for the ordinary observation-only path.
    if (AXIsProcessTrusted()) {
        CGEventMask buttons = CGEventMaskBit(kCGEventLeftMouseDown) |
                              CGEventMaskBit(kCGEventRightMouseDown) |
                              CGEventMaskBit(kCGEventLeftMouseUp) | CGEventMaskBit(kCGEventRightMouseUp);
        s->tap = CGEventTapCreate(kCGSessionEventTap, kCGHeadInsertEventTap, kCGEventTapOptionDefault,
                                  buttons, captureButtons, (__bridge void *)s);
        if (s->tap) {
            s->tapSource = CFMachPortCreateRunLoopSource(NULL, s->tap, 0);
            CFRunLoopAddSource(CFRunLoopGetMain(), s->tapSource, kCFRunLoopCommonModes);
        }
    }
    mask |= NSEventMaskLeftMouseDown | NSEventMaskRightMouseDown;
    void (^record)(NSEvent *) = ^(NSEvent *e) {
      switch (e.type) {
      case NSEventTypeRightMouseDown:
          s->actions |= 2;
          break;
      case NSEventTypeLeftMouseDown:
          s->clickPoint = e.CGEvent ? finderEventPoint(s, e.CGEvent) : finderPosition(s);
          s->actions |= 1;
          break;
      case NSEventTypeScrollWheel:
          s->revision++;
          break;
      default:
          break;
      }
    };
    s->globalMonitor = [NSEvent addGlobalMonitorForEventsMatchingMask:mask handler:record];
    s->localMonitor = [NSEvent
        addLocalMonitorForEventsMatchingMask:mask | NSEventMaskKeyDown
                                     handler:^NSEvent *(NSEvent *e) {
                                       if (e.type == NSEventTypeKeyDown && e.keyCode == kVK_Escape) {
                                           s->actions |= 2;
                                           return nil;
                                       }
                                       record(e);
                                       return e;
                                     }];
    NSNotificationCenter *center = NSWorkspace.sharedWorkspace.notificationCenter;
    void (^changed)(NSNotification *) = ^(NSNotification *n) {
      (void)n;
      s->revision++;
    };
    s->activation = [center addObserverForName:NSWorkspaceDidActivateApplicationNotification
                                        object:nil
                                         queue:NSOperationQueue.mainQueue
                                    usingBlock:changed];
    s->space = [center addObserverForName:NSWorkspaceActiveSpaceDidChangeNotification
                                   object:nil
                                    queue:NSOperationQueue.mainQueue
                               usingBlock:changed];
    EventTypeSpec type = {kEventClassKeyboard, kEventHotKeyPressed};
    EventHotKeyID key = {'XrQR', 1};
    if (InstallEventHandler(GetApplicationEventTarget(), finderEscape, 1, &type, (__bridge void *)s,
                            &s->handler) == noErr)
        RegisterEventHotKey(kVK_Escape, 0, key, GetApplicationEventTarget(), 0, &s->escape);
    allowBackgroundCursor(s);
    xrose_finder_cursor((__bridge void *)s, false);
    return (__bridge_retained void *)s;
}
unsigned int xrose_finder_input(void *ptr, float *x, float *y, float *clickX, float *clickY,
                                unsigned int *revision) {
    XroseFinderInput *s = (__bridge XroseFinderInput *)ptr;
    // Mouse event counters survive short clicks and applications with their own
    // event loops. AppKit monitors supply the event position when available.
    uint32_t left =
        CGEventSourceCounterForEventType(kCGEventSourceStateCombinedSessionState, kCGEventLeftMouseDown);
    uint32_t right = CGEventSourceCounterForEventType(kCGEventSourceStateCombinedSessionState,
                                                      kCGEventRightMouseDown);
    uint32_t scroll =
        CGEventSourceCounterForEventType(kCGEventSourceStateCombinedSessionState, kCGEventScrollWheel);
    if (left != s->leftCount && !(s->actions & 1)) {
        s->clickPoint = finderPosition(s);
        s->actions |= 1;
    }
    if (right != s->rightCount ||
        CGEventSourceKeyState(kCGEventSourceStateCombinedSessionState, kVK_Escape))
        s->actions |= 2;
    if (scroll != s->scrollCount)
        s->revision++;
    s->leftCount = left;
    s->rightCount = right;
    s->scrollCount = scroll;
    CGPoint p = finderPosition(s);
    *x = p.x;
    *y = p.y;
    *clickX = s->clickPoint.x;
    *clickY = s->clickPoint.y;
    *revision = s->revision;
    unsigned int result = s->actions;
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
    if (s->escape)
        UnregisterEventHotKey(s->escape);
    if (s->handler)
        RemoveEventHandler(s->handler);
    NSNotificationCenter *center = NSWorkspace.sharedWorkspace.notificationCenter;
    [center removeObserver:s->activation];
    [center removeObserver:s->space];
    s->activation = nil;
    s->space = nil;
    xrose_finder_cursor((__bridge void *)s, true);
    if (s->setConnectionProperty)
        s->setConnectionProperty(s->cursorConnection, s->cursorConnection, CFSTR("SetsCursorInBackground"),
                                 (__bridge CFTypeRef)(s->previousCursorPolicy ?: @NO));
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
