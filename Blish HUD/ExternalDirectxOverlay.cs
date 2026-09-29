// Originally written by SorryQuick for https://github.com/SorryQuick/Blish-HUD, as the
// Blish HUD side of external-dx11-overlay. Modified in this fork: mouse buttons and the
// wheel now arrive over UDP alongside movement, and the header gained a block-mouse flag.

using Blish_HUD.Input;
using Microsoft.Xna.Framework.Graphics;
using Microsoft.Xna.Framework.Input;
using SharpDX;
using SharpDX.Direct3D11;
using SharpDX.DXGI;
using System;
using System.Collections.Concurrent;
using System.Globalization;
using System.IO;
using System.IO.MemoryMappedFiles;
using System.Linq.Expressions;
using System.Net;
using System.Net.Sockets;
using System.Reflection;
using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Threading;
using Color = Microsoft.Xna.Framework.Color;
using MouseEventArgs = Blish_HUD.Input.MouseEventArgs;
using Texture2D = SharpDX.Direct3D11.Texture2D;

namespace Blish_HUD {
    internal static class ExternalDirectxOverlay {
        private static Thread _udpListenerThread;
        private static volatile bool _listening;
        private const int LISTENPORT = 49152;


        //-------------------------------------------- RENDERING ---------------------------------------------//
        public static MemoryMappedFile HeaderMMF = null;
        public static MemoryMappedViewAccessor HeaderAccesor = null;
        public static int Width = 0 ;
        public static int Height = 0;
        const string HEADERMAPNAME = "BlishHUD_Header";
        const int HEADERSIZE = 32;
        const int HEADEROFFSET_BLOCKMOUSE = 28;
        private static uint _textureIdx = 0;


        private static readonly object _writeLock = new object();

        //Double buffer for synchronicity
        private static Texture2D[] _textures2D;
        private static SharpDX.Direct3D11.Device _device;
        private static SwapChain _swapChain;
        public static IntPtr[] SharedTextureHandles;

        //Globals
        public static bool AutoUpdatesEnabled = false;

        //Mutex the rust side can use to check if Blish is still running.
        private static Mutex _isAliveMtx = new Mutex(true, "Global\\blish_isalive_mutex");

        //Set by blish when a new frame is available, prevents having to sleep arbitrary amounts of time
        private static EventWaitHandle _wakeEvent = new EventWaitHandle(false, EventResetMode.AutoReset, "Global\\BlishHUD_WakeEvent");

        //Sent by the dll when the game window has resized. Allows better accuracy in resolution
        private static EventWaitHandle _resizeEvent = new EventWaitHandle(false, EventResetMode.ManualReset, "Global\\BlishHUD_ResizeEvent");

        //Because for some reason I can't make it work peroperly with GameService.Overlay.InterfaceHidden
        public static volatile bool InterfaceHidden = false;


        /*
            Header : [ width (u32) | height (u32) | index (u32) | sharedtextureptr1 (u64) | sharedtextureptr2 (u64) | blockmouse (u32)]
            Body: [ Full Frame ]

            blockmouse is written by us every frame and read by the dll: non-zero means the
            cursor is over one of our controls, so the dll should swallow the click instead
            of passing it to the game.
         */

        private static readonly object _logLock = new object();
        private static string _logPath;
        //simple logging function to write debug messages to a file
        public static void Log(string level, string message, Exception ex = null) {
            if (_logPath == null) {
                var logsDir = Path.Combine(AppContext.BaseDirectory, "..", "logs");
                Directory.CreateDirectory(logsDir);

                string timestamp = DateTime.Now.ToString("yyyy-MM-dd_HH-mm-ss", CultureInfo.InvariantCulture);
                _logPath = Path.Combine(logsDir, $"BlishHUD-{timestamp}.log");
            }

            var now = DateTime.Now.ToString("yyyy-MM-dd HH:mm:ss", CultureInfo.InvariantCulture);
            var logEntry = $"[{now}] [BlishHUD] [{level.ToUpper()}] {message}";

            if (ex != null) {
                logEntry += Environment.NewLine + ex + Environment.NewLine;
            }

            lock (_logLock) {
                File.AppendAllText(_logPath, logEntry + Environment.NewLine);
            }
            Console.WriteLine(logEntry);
        }

        public static void ResizeTextures(GraphicsDevice device, int width, int height) {
            try { 
                device.SetRenderTarget(null);

                var oldTextures = _textures2D;

                var newRenderTarget = new RenderTarget2D(
                    device,
                    width,
                    height,
                    false,
                    SurfaceFormat.Color,
                    DepthFormat.None,
                    0,
                    RenderTargetUsage.DiscardContents
                );

                var desc = new Texture2DDescription {
                    Width = width,
                    Height = height,
                    MipLevels = 1,
                    ArraySize = 1,
                    Format = Format.R8G8B8A8_UNorm,
                    SampleDescription = new SampleDescription(1, 0),
                    Usage = ResourceUsage.Default,
                    BindFlags = BindFlags.ShaderResource | BindFlags.RenderTarget,
                    CpuAccessFlags = CpuAccessFlags.None,
                    OptionFlags = ResourceOptionFlags.SharedKeyedmutex
                };
                _device = (SharpDX.Direct3D11.Device)typeof(GraphicsDevice).GetField("_d3dDevice", BindingFlags.NonPublic | BindingFlags.Instance).GetValue(device);

                //LogDebugInfo();

                _swapChain = (SwapChain)typeof(GraphicsDevice).GetField("_swapChain", BindingFlags.NonPublic | BindingFlags.Instance).GetValue(device);

                var newTextures = new Texture2D[] { new Texture2D(_device, desc), new Texture2D(_device, desc) };
                var newHandles = new IntPtr[newTextures.Length];

                for (int i = 0; i < newTextures.Length; i++) {
                    using var dxgiResource = newTextures[i].QueryInterface<SharpDX.DXGI.Resource>();
                    newHandles[i] = dxgiResource.SharedHandle;
                }

                HeaderAccesor.Write(8, _textureIdx);
                HeaderAccesor.Write(12, newHandles[0].ToInt64());
                HeaderAccesor.Write(20, newHandles[1].ToInt64());

                //Swap in new textures
                _textures2D = newTextures;
                SharedTextureHandles = newHandles;

                // Dispose old stuff
                if (oldTextures != null) {
                    foreach (var t in oldTextures) t?.Dispose();
                }
                //Notify the dll
                _wakeEvent.Set();
            } catch (Exception ex) {
                Log("ERROR", "Could not create shared textures. If you are using DXVK, make sure you are using 1.10.1 or more recent. " +
                    "If you are on MAC, try release 0.7 on github instead.", ex);
                throw;
            }
        }

        private static void LogDebugInfo() {
            var supportbgra = _device.CheckFormatSupport(Format.B8G8R8A8_UNorm);
            var supportrbga = _device.CheckFormatSupport(Format.R8G8B8A8_UNorm);
            Log("debug", $"BGRA support: {supportbgra}");
            Log("debug", $"RGBA support: {supportrbga}");
            Log("debug", "Feature Level (Upwards of minimum 40960 required): " + _device.FeatureLevel);
        }

        public static void CopyToSharedTexture(GraphicsDevice device) {
            if (HeaderAccesor == null) {
                initializeMMF();
            }

            UpdateMouseBlockFlag();
            if (_resizeEvent.WaitOne(0)) {
                uint w = 0;
                uint h = 0;
                HeaderAccesor.Read(0, out w);
                HeaderAccesor.Read(4, out h);
                Width = (int)w;
                Height = (int)h;
                if (w == 0 && h == 0) {
                    return;
                }
                ResizeTextures(device, (int)w, (int)h);
                _resizeEvent.Reset();
            }
            if (SharedTextureHandles != null) { 
                try {
                    var texture = _swapChain.GetBackBuffer<Texture2D>(0);

                    //Because the source texture is multisampled.
                    //Basically a copy.
                    _device.ImmediateContext.ResolveSubresource(
                        texture,
                        0,
                        _textures2D[_textureIdx],
                        0,
                        Format.R8G8B8A8_UNorm
                    );

                    _device.ImmediateContext.Flush();
                    FlipBufferIdx();
                } catch (Exception e) {
                    Log("debug", $"Failed to copy textures");
                }
            }
        }

        //Tells the dll whether the cursor is currently over one of our controls, so it can keep
        //that click from also landing in the game. This replaces the swallow-the-event return
        //value of the old WH_MOUSE_LL hook. Written every frame, since the dll reads it from
        //the game's UI thread the moment a button message arrives.
        private static void UpdateMouseBlockFlag() {
            var activeControl = GameService.Input.Mouse.ActiveControl;

            bool block = activeControl != null
                      && !activeControl.Captures.HasFlag(Blish_HUD.Controls.CaptureType.DoNotBlock)
                      && !InterfaceHidden;

            HeaderAccesor.Write(HEADEROFFSET_BLOCKMOUSE, block ? 1u : 0u);
        }

        private static void FlipBufferIdx() {
            _textureIdx ^= 1;
            HeaderAccesor.Write(8, _textureIdx);

            //Notify the dll
            _wakeEvent.Set();
        }
        
        private static void initializeMMF() {
            HeaderMMF = MemoryMappedFile.CreateOrOpen(HEADERMAPNAME, HEADERSIZE, MemoryMappedFileAccess.ReadWrite);
            HeaderAccesor = HeaderMMF.CreateViewAccessor(0, HEADERSIZE, MemoryMappedFileAccess.ReadWrite);

            GameService.Overlay.HideAllInterface.Value.Activated += (sender, e) => {
                if (!InterfaceHidden) {
                    ClearTextures();
                    InterfaceHidden = true;
                } else {
                    InterfaceHidden = false;
                }
            };
            //Notify the dll we're ready
            _wakeEvent.Set();
            Log("Debug", "BlishHUD started successfully.");
        }

        //Clears the textures when the overlay is hidden.
        private static void ClearTextures() {
            if (_textures2D == null) return;

            foreach (var tex in _textures2D) {
                if (tex == null) continue;

                using (var rtv = new RenderTargetView(_device, tex)) {
                    var clearColor = new Color4(0, 0, 0, 0);
                    _device.ImmediateContext.ClearRenderTargetView(rtv, clearColor);
                }
            }
        }


        //---------------------------------------------- INPUT -----------------------------------------------//

        public static void StartUdpServer() {
            _listening = true;
            _udpListenerThread = new Thread(UdpListenLoop) {
                IsBackground = true
            };
            _udpListenerThread.Start();
        }

        //TODO: use this
        public static void StopUdpServer() {
            _listening = false;
            _udpListenerThread?.Join();
        }

        //Message format: [msg(4 bytes), x(4 bytes), y(4 bytes), mouseData(4 bytes)]
        //
        //msg is the raw Win32 WM_* id from the dll's wnd_proc. Blish's MouseEventType is
        //defined with those same values (MouseMoved == WM_MOUSEMOVE == 512), so it casts
        //straight across. mouseData is the original wparam, which is where WM_MOUSEWHEEL
        //keeps its delta -- MouseEventArgs.WheelDelta reads it out of the high word.
        //
        //This carries buttons and the wheel as well as movement, which is what lets Blish
        //run without a global WH_MOUSE_LL hook. That hook sat in the path of every cursor
        //warp the game makes during camera look and corrupted its pointer tracking on wine.
        private const int MOUSE_PACKET_SIZE = 16;

        //Virtual key flags carried in the low word of wparam on every mouse message.
        private const int MK_LBUTTON  = 0x0001;
        private const int MK_RBUTTON  = 0x0002;
        private const int MK_MBUTTON  = 0x0010;
        private const int MK_XBUTTON1 = 0x0020;
        private const int MK_XBUTTON2 = 0x0040;

        private readonly struct MousePacket {
            public readonly MouseEventType EventType;
            public readonly int            X;
            public readonly int            Y;
            public readonly int            MouseData;

            public MousePacket(MouseEventType eventType, int x, int y, int mouseData) {
                this.EventType = eventType;
                this.X         = x;
                this.Y         = y;
                this.MouseData = mouseData;
            }
        }

        private static readonly ConcurrentQueue<MousePacket> _mouseEvents = new ConcurrentQueue<MousePacket>();

        //Only a backstop against unbounded growth if the update thread stalls; a drop here is
        //harmless because button state is rebuilt from wparam on the next packet.
        private const int MAX_QUEUED_MOUSE_EVENTS = 512;

        private static void UdpListenLoop() {
            using (var udpClient = new UdpClient(LISTENPORT)) {
                var remoteEP = new IPEndPoint(IPAddress.Any, 0);
                while (_listening) {
                    try {
                        byte[] data = udpClient.Receive(ref remoteEP);
                        if (data.Length >= MOUSE_PACKET_SIZE) {
                            var packet = new MousePacket(
                                (MouseEventType)BitConverter.ToUInt32(data, 0),
                                BitConverter.ToInt32(data, 4),
                                BitConverter.ToInt32(data, 8),
                                BitConverter.ToInt32(data, 12));

                            _mouseEvents.Enqueue(packet);

                            while (_mouseEvents.Count > MAX_QUEUED_MOUSE_EVENTS && _mouseEvents.TryDequeue(out _)) { }
                        }
                    } catch (SocketException) {
                        Log("Error", "SocketException occurred while receiving UDP data.");
                    }
                }
            }
        }

        /// <summary>
        /// Applies the mouse events received since the last frame. Must be called from the
        /// update thread: applying an event reaches <see cref="Input.MouseHandler.HandleInput"/>,
        /// which reads <c>Form.ActiveForm</c>, and that may not be touched off the UI thread.
        /// Keeping the socket thread to parse-and-queue also stops a burst of movement from
        /// overflowing the receive buffer while an event is being handled.
        /// </summary>
        internal static void DrainMouseEvents() {
            while (_mouseEvents.TryDequeue(out var packet)) {
                ApplyMouseEvent(packet.EventType, packet.X, packet.Y, packet.MouseData);
            }
        }

        private static ButtonState ButtonFromFlags(int flags, int mask) {
            return (flags & mask) != 0 ? ButtonState.Pressed : ButtonState.Released;
        }

        //Blish only has MouseEventType values for these; the dll also forwards the middle
        //button, which still updates button state but has nothing to dispatch to.
        private static bool IsDispatchable(MouseEventType eventType) {
            switch (eventType) {
                case MouseEventType.MouseMoved:
                case MouseEventType.LeftMouseButtonPressed:
                case MouseEventType.LeftMouseButtonReleased:
                case MouseEventType.RightMouseButtonPressed:
                case MouseEventType.RightMouseButtonReleased:
                case MouseEventType.MouseWheelScrolled:
                    return true;
                default:
                    return false;
            }
        }

        private static void ApplyMouseEvent(MouseEventType eventType, int x, int y, int mouseData) {
            MouseState old = GameService.Input.Mouse.StaticMouseState;

            //Button state is rebuilt from the flags wparam carries on every message rather than
            //toggled per press/release event. UDP gives no delivery guarantee even on loopback,
            //and a lost press/release would otherwise leave a button stuck for as long as the
            //user kept playing; derived this way, the very next message corrects it.
            int flags = mouseData & 0xFFFF;

            //WM_MOUSEWHEEL is the one message here whose lparam is in screen coordinates, so it
            //must not be allowed to move our idea of the cursor.
            bool positionValid = eventType != MouseEventType.MouseWheelScrolled;

            //Note the argument order: XNA's MouseState takes left, MIDDLE, right.
            GameService.Input.Mouse.StaticMouseState = new MouseState(
                positionValid ? x : old.X,
                positionValid ? y : old.Y,
                old.ScrollWheelValue,
                ButtonFromFlags(flags, MK_LBUTTON),
                ButtonFromFlags(flags, MK_MBUTTON),
                ButtonFromFlags(flags, MK_RBUTTON),
                ButtonFromFlags(flags, MK_XBUTTON1),
                ButtonFromFlags(flags, MK_XBUTTON2));

            if (!IsDispatchable(eventType)) return;

            GameService.Input.Mouse.HandleInput(
                new MouseEventArgs(eventType, x, y, mouseData, 0, Environment.TickCount, 0));
        }

        //--------------------------------------------- WINDOW MANAGEMENT ------------------------------------------//

        [DllImport("user32.dll", SetLastError = true)]
        static extern IntPtr SetWindowLongPtr(IntPtr hWnd, int nIndex, WndProcDelegate dwNewLong);

        [DllImport("user32.dll", SetLastError = true)]
        static extern IntPtr GetWindowLongPtr(IntPtr hWnd, int nIndex);
        [DllImport("user32.dll")]
        static extern IntPtr CallWindowProc(IntPtr lpPrevWndFunc, IntPtr hWnd, uint Msg, IntPtr wParam, IntPtr lParam);

        [StructLayout(LayoutKind.Sequential)]
        struct WINDOWPOS {
            public IntPtr hwnd;
            public IntPtr hwndInsertAfter;
            public int x;
            public int y;
            public int cx;
            public int cy;
            public uint flags;
        }

        const int GWL_WNDPROC = -4;
        const int WM_SHOWWINDOW = 0x0018;
        const int WM_WINDOWPOSCHANGING = 0x0046;
        const uint SWP_HIDEWINDOW = 0x0080;
        const uint SWP_SHOWWINDOW = 0x0040;

        delegate IntPtr WndProcDelegate(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam);

        static WndProcDelegate newWndProc;
        static IntPtr originalWndProc;

        public static void setupNewWndProc() {
            IntPtr hwnd = BlishHud.Instance.FormHandle;
            originalWndProc = GetWindowLongPtr(hwnd, GWL_WNDPROC);
            newWndProc = HookWndProc;
            SetWindowLongPtr(hwnd, GWL_WNDPROC, newWndProc);
            BlishHud.Instance.Form.Visible = false;
            BlishHud.Instance.Form.Hide();
        }
        static IntPtr CallOriginalWndProc(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam) {
            return CallWindowProc(originalWndProc, hWnd, msg, wParam, lParam);
        }


        //Prevents the window from ever showing up, even if Blish tries to show it.
        static IntPtr HookWndProc(IntPtr hWnd, uint msg, IntPtr wParam, IntPtr lParam) {
            if (msg == WM_SHOWWINDOW) {
                return IntPtr.Zero;
            } else if (msg == WM_WINDOWPOSCHANGING) {
                unsafe {
                    WINDOWPOS* pos = (WINDOWPOS*)lParam;
                    if ((pos->flags & SWP_SHOWWINDOW) != 0) {
                        pos->flags &= ~SWP_SHOWWINDOW;
                        pos->flags |= SWP_HIDEWINDOW;
                    }
                }
            }

            return CallOriginalWndProc(hWnd, msg, wParam, lParam);
        }

        //This patches audio to work on linux. Basically it stubs the methods that wine does not implement.
        //Using Harmony would be 1000% better, but it did not seem feasible due to .Net versions.

        [DllImport("kernel32")]
        private static extern bool VirtualProtect(IntPtr lpAddress, UIntPtr dwSize, uint flNewProtect, out uint lpflOldProtect);

        public static void PatchUnregisterNotifications() {
            try {
                var method = typeof(NAudio.CoreAudioApi.AudioSessionManager)
                                .GetMethod("UnregisterNotifications", BindingFlags.Instance | BindingFlags.NonPublic);
                if (method == null) return;

                //Force compile
                RuntimeHelpers.PrepareMethod(method.MethodHandle);

                IntPtr ptr = method.MethodHandle.GetFunctionPointer();

                //0xC3 = ret
                //Only on x86_64 and probably x86.
                byte[] patch = { 0xC3 };

                //Make memory writable
                VirtualProtect(ptr, (UIntPtr)patch.Length, 0x40, out uint oldProtect);

                //Patch
                Marshal.Copy(patch, 0, ptr, patch.Length);

                //Restore protect
                VirtualProtect(ptr, (UIntPtr)patch.Length, oldProtect, out _);

            } catch (Exception ex) {
                Log("Debug", "Failed to patch audio", ex);
            }
        }
    }
}