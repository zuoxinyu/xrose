#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <shellapi.h>
#include <tlhelp32.h>
#include <wincodec.h>
#include <wchar.h>

// Name-only metadata can still identify a running image without searching the
// current working directory for an unrelated executable of the same name.
static BOOL running_image(const wchar_t *name, wchar_t *path, DWORD capacity) {
    HANDLE snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
    if (snapshot == INVALID_HANDLE_VALUE) return FALSE;
    PROCESSENTRY32W entry = {0};
    entry.dwSize = sizeof(entry);
    BOOL found = FALSE;
    for (BOOL more = Process32FirstW(snapshot, &entry); more && !found;
         more = Process32NextW(snapshot, &entry)) {
        if (_wcsicmp(entry.szExeFile, name) != 0) continue;
        HANDLE process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, FALSE, entry.th32ProcessID);
        if (!process) continue;
        DWORD length = capacity;
        found = QueryFullProcessImageNameW(process, 0, path, &length);
        CloseHandle(process);
    }
    CloseHandle(snapshot);
    return found;
}

static HICON executable_icon(const wchar_t *key) {
    wchar_t resolved[32768];
    if (!wcspbrk(key, L"\\/:")) {
        if (!running_image(key, resolved, ARRAYSIZE(resolved))) return NULL;
        key = resolved;
    }
    if (wcsncmp(key, L"\\\\?\\", 4) == 0) key += 4;
    // Only open local executable images; connection metadata may contain UNC paths.
    if (!((key[0] >= L'A' && key[0] <= L'Z') || (key[0] >= L'a' && key[0] <= L'z')) ||
        key[1] != L':' || (key[2] != L'\\' && key[2] != L'/')) return NULL;
    const wchar_t *ext = wcsrchr(key, L'.');
    if (!ext || _wcsicmp(ext, L".exe") != 0) return NULL;
    wchar_t drive[] = { key[0], L':', L'\\', 0 };
    if (GetDriveTypeW(drive) == DRIVE_REMOTE) return NULL;
    HICON icon = NULL;
    ExtractIconExW(key, 0, &icon, NULL, 1);
    return icon;
}

int xrose_process_icon(const wchar_t *key, BYTE *pixels, int size) {
    HICON icon = *key ? executable_icon(key) : LoadIconW(NULL, MAKEINTRESOURCEW(32512));
    if (!icon) return 0;
    HRESULT apartment = CoInitializeEx(NULL, COINIT_MULTITHREADED);
    IWICImagingFactory *factory = NULL;
    IWICBitmap *bitmap = NULL;
    IWICBitmapScaler *scaler = NULL;
    IWICBitmapSource *bgra = NULL;
    HRESULT hr = CoCreateInstance(&CLSID_WICImagingFactory, NULL, CLSCTX_INPROC_SERVER,
                                   &IID_IWICImagingFactory, (void **)&factory);
    if (SUCCEEDED(hr)) hr = IWICImagingFactory_CreateBitmapFromHICON(factory, icon, &bitmap);
    if (SUCCEEDED(hr)) hr = IWICImagingFactory_CreateBitmapScaler(factory, &scaler);
    if (SUCCEEDED(hr)) hr = IWICBitmapScaler_Initialize(scaler, (IWICBitmapSource *)bitmap,
                                                       size, size, WICBitmapInterpolationModeFant);
    if (SUCCEEDED(hr)) hr = WICConvertBitmapSource(&GUID_WICPixelFormat32bppBGRA,
                                                   (IWICBitmapSource *)scaler, &bgra);
    if (SUCCEEDED(hr)) hr = IWICBitmapSource_CopyPixels(bgra, NULL, size * 4, size * size * 4, pixels);
    if (bgra) IWICBitmapSource_Release(bgra);
    if (scaler) IWICBitmapScaler_Release(scaler);
    if (bitmap) IWICBitmap_Release(bitmap);
    if (factory) IWICImagingFactory_Release(factory);
    if (SUCCEEDED(apartment)) CoUninitialize();
    if (*key) DestroyIcon(icon);
    return SUCCEEDED(hr);
}
