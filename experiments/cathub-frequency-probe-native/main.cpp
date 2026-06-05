#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cstdint>
#include <cstdio>
#include <deque>
#include <filesystem>
#include <fstream>
#include <iterator>
#include <mutex>
#include <optional>
#include <sstream>
#include <string>
#include <thread>
#include <vector>

namespace {

constexpr wchar_t kWindowClassName[] = L"CatHubFrequencyProbeNativeWindow";
constexpr wchar_t kWindowTitle[] = L"CatHub Native Frequency Probe";
constexpr UINT kUpdateMessage = WM_APP + 1;
constexpr int kPollMilliseconds = 100;
constexpr int kMaxLogLines = 8;

constexpr COLORREF kBackground = RGB(10, 6, 1);
constexpr COLORREF kPanel = RGB(18, 11, 2);
constexpr COLORREF kPanelDark = RGB(7, 4, 1);
constexpr COLORREF kAmber = RGB(255, 191, 0);
constexpr COLORREF kAmberDim = RGB(185, 133, 25);
constexpr COLORREF kAmberBorder = RGB(255, 159, 28);

struct Snapshot {
    std::uint64_t frequency_hz{};
    std::wstring frequency;
    std::wstring vfo;
    std::wstring mode;
    long long query_ms{};
    std::optional<long long> change_gap_ms;
    bool connected{};
    std::wstring error;
};

struct SharedState {
    std::mutex gate;
    Snapshot snapshot;
    std::deque<std::wstring> log_lines;
    std::filesystem::path log_path;
};

std::atomic_bool g_running{true};
SharedState g_state;

std::wstring WidenAscii(const std::string& value) {
    return std::wstring(value.begin(), value.end());
}

std::string NarrowUtf8(const std::wstring& value) {
    if (value.empty()) {
        return {};
    }

    const int bytes = WideCharToMultiByte(CP_UTF8, 0, value.data(),
                                          static_cast<int>(value.size()), nullptr, 0,
                                          nullptr, nullptr);
    if (bytes <= 0) {
        return {};
    }

    std::string result(static_cast<std::size_t>(bytes), '\0');
    WideCharToMultiByte(CP_UTF8, 0, value.data(), static_cast<int>(value.size()),
                        result.data(), bytes, nullptr, nullptr);
    return result;
}

std::wstring FormatFrequency(std::uint64_t frequency_hz) {
    const auto mhz = frequency_hz / 1'000'000;
    const auto khz = (frequency_hz % 1'000'000) / 1'000;
    const auto ten_hz = (frequency_hz % 1'000) / 10;

    wchar_t buffer[32]{};
    std::swprintf(buffer, std::size(buffer), L"%llu.%03llu.%02llu",
                 static_cast<unsigned long long>(mhz),
                 static_cast<unsigned long long>(khz),
                 static_cast<unsigned long long>(ten_hz));
    return buffer;
}

std::wstring NowTime() {
    SYSTEMTIME now{};
    GetLocalTime(&now);
    wchar_t buffer[32]{};
    std::swprintf(buffer, std::size(buffer), L"%02u:%02u:%02u.%03u",
                 now.wHour, now.wMinute, now.wSecond, now.wMilliseconds);
    return buffer;
}

std::filesystem::path LogPath() {
    wchar_t buffer[MAX_PATH]{};
    const auto length = GetEnvironmentVariableW(L"LOCALAPPDATA", buffer, MAX_PATH);
    std::filesystem::path root = length == 0 ? std::filesystem::temp_directory_path()
                                             : std::filesystem::path(buffer);
    auto directory = root / L"qsoripper";
    std::filesystem::create_directories(directory);
    return directory / L"cathub-frequency-probe-native.log";
}

void AppendLog(const std::wstring& message) {
    const auto line = NowTime() + L" " + message;
    {
        std::lock_guard lock(g_state.gate);
        g_state.log_lines.push_back(line);
        while (g_state.log_lines.size() > kMaxLogLines) {
            g_state.log_lines.pop_front();
        }
    }

    std::ofstream file(g_state.log_path, std::ios::app);
    file << NarrowUtf8(line) << '\n';
}

class WinSockSession {
public:
    WinSockSession() {
        WSADATA data{};
        ok_ = WSAStartup(MAKEWORD(2, 2), &data) == 0;
    }

    ~WinSockSession() {
        if (ok_) {
            WSACleanup();
        }
    }

    WinSockSession(const WinSockSession&) = delete;
    WinSockSession& operator=(const WinSockSession&) = delete;

    [[nodiscard]] bool ok() const {
        return ok_;
    }

private:
    bool ok_{};
};

class CatHubClient {
public:
    ~CatHubClient() {
        Disconnect();
    }

    CatHubClient(const CatHubClient&) = delete;
    CatHubClient& operator=(const CatHubClient&) = delete;

    CatHubClient() = default;

    Snapshot ReadSnapshot() {
        const auto start = std::chrono::steady_clock::now();
        EnsureConnected();

        const auto frequency_line = CommandLine("f");
        const auto mode = CommandLine("m");
        const auto ignored_passband = CommandLineRaw();
        (void)ignored_passband;
        const auto vfo = CommandLine("v");
        const auto elapsed = std::chrono::duration_cast<std::chrono::milliseconds>(
            std::chrono::steady_clock::now() - start);

        std::uint64_t frequency = 0;
        try {
            frequency = std::stoull(frequency_line);
        } catch (...) {
            throw std::runtime_error("invalid frequency reply: " + frequency_line);
        }

        Snapshot snapshot;
        snapshot.frequency_hz = frequency;
        snapshot.frequency = FormatFrequency(frequency);
        snapshot.mode = WidenAscii(mode);
        snapshot.vfo = WidenAscii(vfo);
        snapshot.query_ms = elapsed.count();
        snapshot.connected = true;
        return snapshot;
    }

private:
    SOCKET socket_{INVALID_SOCKET};

    void EnsureConnected() {
        if (socket_ != INVALID_SOCKET) {
            return;
        }

        addrinfo hints{};
        hints.ai_family = AF_INET;
        hints.ai_socktype = SOCK_STREAM;
        hints.ai_protocol = IPPROTO_TCP;

        addrinfo* result = nullptr;
        if (getaddrinfo("127.0.0.1", "4532", &hints, &result) != 0) {
            throw std::runtime_error("getaddrinfo failed");
        }

        SOCKET candidate = INVALID_SOCKET;
        for (auto* ptr = result; ptr != nullptr; ptr = ptr->ai_next) {
            candidate = socket(ptr->ai_family, ptr->ai_socktype, ptr->ai_protocol);
            if (candidate == INVALID_SOCKET) {
                continue;
            }

            DWORD timeout_ms = 500;
            setsockopt(candidate, SOL_SOCKET, SO_RCVTIMEO,
                       reinterpret_cast<const char*>(&timeout_ms), sizeof(timeout_ms));
            setsockopt(candidate, SOL_SOCKET, SO_SNDTIMEO,
                       reinterpret_cast<const char*>(&timeout_ms), sizeof(timeout_ms));

            if (connect(candidate, ptr->ai_addr, static_cast<int>(ptr->ai_addrlen)) == 0) {
                socket_ = candidate;
                break;
            }

            closesocket(candidate);
            candidate = INVALID_SOCKET;
        }

        freeaddrinfo(result);

        if (socket_ == INVALID_SOCKET) {
            throw std::runtime_error("connect 127.0.0.1:4532 failed");
        }
    }

    void Disconnect() {
        if (socket_ != INVALID_SOCKET) {
            closesocket(socket_);
            socket_ = INVALID_SOCKET;
        }
    }

    std::string CommandLine(const char* command) {
        const std::string wire = std::string(command) + "\n";
        if (send(socket_, wire.data(), static_cast<int>(wire.size()), 0) == SOCKET_ERROR) {
            Disconnect();
            throw std::runtime_error("send failed");
        }

        return CommandLineRaw();
    }

    std::string CommandLineRaw() {
        std::string line;
        char ch = '\0';
        while (true) {
            const auto read = recv(socket_, &ch, 1, 0);
            if (read <= 0) {
                Disconnect();
                throw std::runtime_error("recv failed");
            }
            if (ch == '\n') {
                break;
            }
            if (ch != '\r') {
                line.push_back(ch);
            }
        }
        return line;
    }
};

HFONT MakeFont(int height, int weight = FW_NORMAL) {
    return CreateFontW(-height, 0, 0, 0, weight, FALSE, FALSE, FALSE, DEFAULT_CHARSET,
                       OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, CLEARTYPE_QUALITY,
                       FIXED_PITCH | FF_MODERN, L"Cascadia Mono");
}

void DrawTextInRect(HDC dc, const std::wstring& text, const RECT& rect, HFONT font,
                    COLORREF color, UINT format) {
    const auto old_font = SelectObject(dc, font);
    SetBkMode(dc, TRANSPARENT);
    SetTextColor(dc, color);
    RECT copy = rect;
    DrawTextW(dc, text.c_str(), static_cast<int>(text.size()), &copy, format);
    SelectObject(dc, old_font);
}

void FillSolid(HDC dc, const RECT& rect, COLORREF color) {
    const auto brush = CreateSolidBrush(color);
    FillRect(dc, &rect, brush);
    DeleteObject(brush);
}

void StrokeRoundRect(HDC dc, const RECT& rect, COLORREF border, COLORREF fill,
                     int radius, int width) {
    const auto brush = CreateSolidBrush(fill);
    const auto pen = CreatePen(PS_SOLID, width, border);
    const auto old_brush = SelectObject(dc, brush);
    const auto old_pen = SelectObject(dc, pen);
    RoundRect(dc, rect.left, rect.top, rect.right, rect.bottom, radius, radius);
    SelectObject(dc, old_pen);
    SelectObject(dc, old_brush);
    DeleteObject(pen);
    DeleteObject(brush);
}

void DrawTile(HDC dc, const RECT& rect, const std::wstring& label, const std::wstring& value) {
    StrokeRoundRect(dc, rect, RGB(111, 71, 0), kPanelDark, 10, 1);

    const auto label_font = MakeFont(14, FW_BOLD);
    const auto value_font = MakeFont(30, FW_BOLD);
    RECT label_rect{rect.left + 18, rect.top + 14, rect.right - 14, rect.top + 38};
    RECT value_rect{rect.left + 18, rect.top + 46, rect.right - 14, rect.bottom - 10};
    DrawTextInRect(dc, label, label_rect, label_font, kAmberDim, DT_LEFT | DT_TOP | DT_SINGLELINE);
    DrawTextInRect(dc, value, value_rect, value_font, kAmber, DT_LEFT | DT_TOP | DT_SINGLELINE);
    DeleteObject(label_font);
    DeleteObject(value_font);
}

void Paint(HWND hwnd, HDC target) {
    RECT client{};
    GetClientRect(hwnd, &client);

    const int width = client.right - client.left;
    const int height = client.bottom - client.top;
    HDC memory = CreateCompatibleDC(target);
    HBITMAP bitmap = CreateCompatibleBitmap(target, width, height);
    const auto old_bitmap = SelectObject(memory, bitmap);

    FillSolid(memory, client, kBackground);

    Snapshot snapshot;
    std::vector<std::wstring> log_lines;
    std::filesystem::path log_path;
    {
        std::lock_guard lock(g_state.gate);
        snapshot = g_state.snapshot;
        log_lines.assign(g_state.log_lines.begin(), g_state.log_lines.end());
        log_path = g_state.log_path;
    }

    const auto header_font = MakeFont(24, FW_BOLD);
    const auto small_font = MakeFont(15, FW_BOLD);
    const auto footer_font = MakeFont(14, FW_BOLD);

    RECT header{34, 28, width - 34, 104};
    DrawTextInRect(memory, L"CATHUB NATIVE FREQUENCY PROBE", header, header_font,
                   kAmber, DT_LEFT | DT_TOP | DT_SINGLELINE);
    RECT endpoint{34, 58, width - 34, 90};
    DrawTextInRect(memory, L"native Win32/Winsock - direct 127.0.0.1:4532 - 100 ms poll",
                   endpoint, small_font, RGB(255, 209, 102), DT_LEFT | DT_TOP | DT_SINGLELINE);

    HPEN line_pen = CreatePen(PS_SOLID, 1, kAmberBorder);
    const auto old_pen = SelectObject(memory, line_pen);
    MoveToEx(memory, 0, 118, nullptr);
    LineTo(memory, width, 118);
    SelectObject(memory, old_pen);
    DeleteObject(line_pen);

    const int footer_height = 52;
    const int log_height = 170;
    RECT card{34, 152, width - 34, height - footer_height - log_height - 58};
    if (card.bottom < card.top + 280) {
        card.bottom = card.top + 280;
    }
    StrokeRoundRect(memory, card, kAmberBorder, kPanel, 26, 2);

    const auto label_font = MakeFont(20, FW_BOLD);
    RECT live_label{card.left + 42, card.top + 36, card.right - 42, card.top + 70};
    DrawTextInRect(memory, L"LIVE RADIO FREQUENCY", live_label, label_font, kAmberDim,
                   DT_LEFT | DT_TOP | DT_SINGLELINE);

    const int card_height = static_cast<int>(card.bottom - card.top);
    const int freq_font_size = std::max(70, std::min(156, card_height / 3));
    const auto freq_font = MakeFont(freq_font_size, FW_HEAVY);
    const auto mhz_font = MakeFont(42, FW_BOLD);
    RECT frequency_rect{card.left + 42, card.top + 86, card.right - 160, card.top + 215};
    RECT mhz_rect{card.right - 160, card.top + 150, card.right - 34, card.top + 215};
    const auto frequency = snapshot.connected && snapshot.frequency_hz != 0
        ? snapshot.frequency
        : L"--.---.--";
    DrawTextInRect(memory, frequency, frequency_rect, freq_font, kAmber,
                   DT_CENTER | DT_VCENTER | DT_SINGLELINE);
    DrawTextInRect(memory, L"MHz", mhz_rect, mhz_font, RGB(216, 137, 0),
                   DT_LEFT | DT_VCENTER | DT_SINGLELINE);

    const int tile_top = card.bottom - 118;
    const int gap = 18;
    const int tile_width = (card.right - card.left - 84 - (gap * 3)) / 4;
    RECT tile{card.left + 42, tile_top, card.left + 42 + tile_width, card.bottom - 38};
    DrawTile(memory, tile, L"VFO", snapshot.vfo.empty() ? L"--" : snapshot.vfo);
    OffsetRect(&tile, tile_width + gap, 0);
    DrawTile(memory, tile, L"MODE", snapshot.mode.empty() ? L"--" : snapshot.mode);
    OffsetRect(&tile, tile_width + gap, 0);
    DrawTile(memory, tile, L"QUERY", std::to_wstring(snapshot.query_ms) + L" ms");
    OffsetRect(&tile, tile_width + gap, 0);
    DrawTile(memory, tile, L"CHANGE GAP",
             snapshot.change_gap_ms ? std::to_wstring(*snapshot.change_gap_ms) + L" ms" : L"-- ms");

    RECT log_box{34, height - footer_height - log_height - 26, width - 34, height - footer_height - 24};
    StrokeRoundRect(memory, log_box, RGB(111, 71, 0), kPanelDark, 0, 1);
    RECT log_path_rect{log_box.left + 18, log_box.top + 14, log_box.right - 18, log_box.top + 36};
    DrawTextInRect(memory, L"log: " + log_path.wstring(), log_path_rect, small_font, kAmberDim,
                   DT_LEFT | DT_TOP | DT_SINGLELINE | DT_END_ELLIPSIS);
    int y = log_box.top + 46;
    for (const auto& line : log_lines) {
        RECT line_rect{log_box.left + 18, y, log_box.right - 18, y + 19};
        DrawTextInRect(memory, line, line_rect, small_font, RGB(255, 209, 102),
                       DT_LEFT | DT_TOP | DT_SINGLELINE | DT_END_ELLIPSIS);
        y += 18;
    }

    RECT footer{0, height - footer_height, width, height};
    FillSolid(memory, footer, RGB(20, 13, 3));
    std::wstring footer_text = snapshot.connected
        ? L"polling direct cathub - frequency=" + frequency + L" MHz - query=" +
              std::to_wstring(snapshot.query_ms) + L" ms"
        : L"link down - " + snapshot.error;
    RECT footer_text_rect{34, height - 34, width - 34, height - 10};
    DrawTextInRect(memory, footer_text, footer_text_rect, footer_font, kAmberDim,
                   DT_LEFT | DT_TOP | DT_SINGLELINE | DT_END_ELLIPSIS);

    BitBlt(target, 0, 0, width, height, memory, 0, 0, SRCCOPY);

    DeleteObject(header_font);
    DeleteObject(small_font);
    DeleteObject(footer_font);
    DeleteObject(label_font);
    DeleteObject(freq_font);
    DeleteObject(mhz_font);
    SelectObject(memory, old_bitmap);
    DeleteObject(bitmap);
    DeleteDC(memory);
}

void PollLoop(HWND hwnd) {
    WinSockSession winsock;
    if (!winsock.ok()) {
        AppendLog(L"WSAStartup failed");
        return;
    }

    CatHubClient client;
    std::optional<std::uint64_t> last_frequency;
    std::optional<std::chrono::steady_clock::time_point> last_change;
    int poll_count = 0;

    AppendLog(L"native probe starting; target=127.0.0.1:4532 poll=100ms commands=f,m,v");

    while (g_running.load()) {
        const auto loop_start = std::chrono::steady_clock::now();
        Snapshot snapshot;
        try {
            snapshot = client.ReadSnapshot();
            poll_count++;

            const bool changed = !last_frequency || *last_frequency != snapshot.frequency_hz;
            if (changed) {
                if (last_change) {
                    snapshot.change_gap_ms = std::chrono::duration_cast<std::chrono::milliseconds>(
                        loop_start - *last_change)
                                                 .count();
                }
                last_frequency = snapshot.frequency_hz;
                last_change = loop_start;

                std::wstringstream message;
                message << L"change #" << poll_count << L": " << snapshot.frequency_hz
                        << L" Hz " << snapshot.frequency << L" MHz vfo=" << snapshot.vfo
                        << L" mode=" << snapshot.mode << L" query=" << snapshot.query_ms
                        << L"ms gap=";
                if (snapshot.change_gap_ms) {
                    message << *snapshot.change_gap_ms << L"ms";
                } else {
                    message << L"--ms";
                }
                AppendLog(message.str());
            } else if (poll_count % 25 == 0) {
                std::wstringstream message;
                message << L"poll #" << poll_count << L": unchanged " << snapshot.frequency_hz
                        << L" Hz query=" << snapshot.query_ms << L"ms";
                AppendLog(message.str());
            }
        } catch (const std::exception& ex) {
            snapshot.connected = false;
            snapshot.error = WidenAscii(ex.what());
            AppendLog(L"poll error: " + snapshot.error);
        }

        {
            std::lock_guard lock(g_state.gate);
            g_state.snapshot = snapshot;
        }
        PostMessageW(hwnd, kUpdateMessage, 0, 0);

        const auto elapsed = std::chrono::duration_cast<std::chrono::milliseconds>(
            std::chrono::steady_clock::now() - loop_start);
        if (elapsed.count() < kPollMilliseconds) {
            std::this_thread::sleep_for(
                std::chrono::milliseconds(kPollMilliseconds - elapsed.count()));
        }
    }
}

LRESULT CALLBACK WindowProc(HWND hwnd, UINT message, WPARAM wparam, LPARAM lparam) {
    switch (message) {
    case WM_CREATE:
        return 0;
    case kUpdateMessage:
        InvalidateRect(hwnd, nullptr, FALSE);
        return 0;
    case WM_PAINT: {
        PAINTSTRUCT paint{};
        const auto dc = BeginPaint(hwnd, &paint);
        Paint(hwnd, dc);
        EndPaint(hwnd, &paint);
        return 0;
    }
    case WM_SIZE:
        InvalidateRect(hwnd, nullptr, FALSE);
        return 0;
    case WM_DESTROY:
        g_running = false;
        PostQuitMessage(0);
        return 0;
    default:
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
}

} // namespace

int WINAPI wWinMain(HINSTANCE instance, HINSTANCE, PWSTR, int show_command) {
    g_state.log_path = LogPath();

    WNDCLASSW wc{};
    wc.lpfnWndProc = WindowProc;
    wc.hInstance = instance;
    wc.lpszClassName = kWindowClassName;
    wc.hCursor = LoadCursorW(nullptr, IDC_ARROW);
    wc.hbrBackground = CreateSolidBrush(kBackground);

    if (RegisterClassW(&wc) == 0) {
        return 1;
    }

    const auto hwnd = CreateWindowExW(
        0,
        kWindowClassName,
        kWindowTitle,
        WS_OVERLAPPEDWINDOW,
        CW_USEDEFAULT,
        CW_USEDEFAULT,
        1500,
        760,
        nullptr,
        nullptr,
        instance,
        nullptr);

    if (hwnd == nullptr) {
        return 1;
    }

    std::thread poll_thread(PollLoop, hwnd);
    ShowWindow(hwnd, show_command);
    UpdateWindow(hwnd);

    MSG message{};
    while (GetMessageW(&message, nullptr, 0, 0) > 0) {
        TranslateMessage(&message);
        DispatchMessageW(&message);
    }

    g_running = false;
    if (poll_thread.joinable()) {
        poll_thread.join();
    }

    return 0;
}
