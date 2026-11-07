#import <AppKit/AppKit.h>
#import <UniformTypeIdentifiers/UniformTypeIdentifiers.h>

int xrose_process_icon(const char *key, unsigned char *pixels, int size) {
    @autoreleasepool {
        NSWorkspace *workspace = NSWorkspace.sharedWorkspace;
        NSString *path = [NSString stringWithUTF8String:key];
        NSImage *icon = nil;
        if (!path.length) {
            icon = [workspace iconForContentType:UTTypeApplicationBundle];
        } else {
            if (!path.isAbsolutePath) {
                for (NSRunningApplication *app in workspace.runningApplications) {
                    if ([app.executableURL.lastPathComponent isEqualToString:path] && app.bundleURL) {
                        path = app.bundleURL.path;
                        break;
                    }
                }
            }
            if (!path.isAbsolutePath) return 0;
            while (path.length > 1 && ![path.pathExtension.lowercaseString isEqualToString:@"app"])
                path = path.stringByDeletingLastPathComponent;
            if (path.length <= 1 || ![NSFileManager.defaultManager fileExistsAtPath:path]) return 0;
            icon = [workspace iconForFile:path];
        }
        NSRect rect = NSMakeRect(0, 0, size, size);
        CGImageRef image = [icon CGImageForProposedRect:&rect context:nil hints:nil];
        if (!image) return 0;
        CGColorSpaceRef space = CGColorSpaceCreateDeviceRGB();
        CGContextRef ctx = CGBitmapContextCreate(pixels, size, size, 8, size * 4, space,
                                                kCGBitmapByteOrder32Little | kCGImageAlphaPremultipliedFirst);
        CGColorSpaceRelease(space);
        if (!ctx) return 0;
        CGContextSetInterpolationQuality(ctx, kCGInterpolationHigh);
        CGContextDrawImage(ctx, CGRectMake(0, 0, size, size), image);
        CGContextRelease(ctx);
        for (int i = 0; i < size * size * 4; i += 4) {
            unsigned a = pixels[i + 3];
            if (a && a != 255)
                for (int c = 0; c < 3; ++c) pixels[i + c] = (pixels[i + c] * 255 + a / 2) / a;
        }
        return 1;
    }
}
