// MegacityRefbenchHarness.cs
//
// 把 megacity-metro 变成一个确定性可复跑的渲染负载。
//
// 放到上游工程的 Assets/Scripts/Refbench/ 下即可, 不改上游任何既有文件。
// 不带 --es scene 启动时本文件完全不激活, APK 就是原版游戏。
//
// 设计边界(与 refbench 一致): 本文件只负责"制造完全一样的渲染负载"并如实记录事实,
// 不测帧时、不算离散度、不做任何判定。测量归 loop_v1 的 raw ftrace 链路。
//
// !! 状态: 未编译验证 !!
// 写这个文件时本机没有 Unity(装不下, 见 carriers/megacity/README.md), 构建机
// magicbook 也还没装。所以这是一份待验证的初稿: API 用法按 Unity 6000.1 文档写,
// 但没过编译器, 更没在设备上跑过。首次在 magicbook 打开工程时以编译器为准。

using System;
using System.Collections;
using System.Collections.Generic;
using System.Globalization;
using System.IO;
using System.Security.Cryptography;
using System.Text;
using UnityEngine;
using UnityEngine.Rendering;
using UnityEngine.Networking;
using UnityEngine.SceneManagement;

namespace Megacity.Refbench
{
    public sealed class MegacityRefbenchHarness : MonoBehaviour
    {
        const int    ContractVersion = 1;
        const string HarnessVersion  = "0.1.0-draft";
        const string Carrier         = "megacity";

        // 与 contract/launch.json 对齐
        const string StartedFileName = "megacity_started";
        const string OutputFileName  = "megacity_out.json";

        // ---- 入口 --------------------------------------------------------
        // AfterSceneLoad: 等第一个场景起来再接管, 免得和上游的 bootstrap 抢时序。
        [RuntimeInitializeOnLoadMethod(RuntimeInitializeLoadType.AfterSceneLoad)]
        static void Boot()
        {
            var scene = GetIntentExtra("scene");
            if (string.IsNullOrEmpty(scene))
                return;   // 正常游玩, 不激活

            var go = new GameObject("[MegacityRefbenchHarness]");
            DontDestroyOnLoad(go);
            go.AddComponent<MegacityRefbenchHarness>();
        }

        // ---- 参数 --------------------------------------------------------
        string m_Scene, m_RunId, m_RouteName, m_VSync, m_UnityScene;
        int    m_Frames, m_Warmup;
        float  m_ResScale;

        Route  m_Route;
        Camera m_Cam;

        int    m_WarmupRendered;
        int    m_Rendered;
        bool   m_CleanExit;
        string m_AbortReason = "";

        void Awake()
        {
            m_Scene     = GetIntentExtra("scene");
            m_RunId     = GetIntentExtra("run_id")   ?? "unset";
            m_RouteName = GetIntentExtra("route")    ?? "route_a";
            m_VSync     = GetIntentExtra("vsync")    ?? "off";
            // 上游启动进的是 Menu 场景, 城市在 Main 里。不切过去就只是在菜单前面摆了台相机。
            m_UnityScene = GetIntentExtra("unity_scene") ?? "Main";
            m_Frames    = ParseInt(GetIntentExtra("frames"),  3600, 60, 100000);
            m_Warmup    = ParseInt(GetIntentExtra("warmup"),   600,  0, 100000);
            m_ResScale  = ParseFloat(GetIntentExtra("resscale"), 1.0f, 0.25f, 2.0f);
        }

        IEnumerator Start()
        {
            // 1) 把一切会让两轮不一样的自适应行为按死
            //    帧率不锁 -> 分位数统计能在固定采集窗内拿到更多帧(与 refbench 同理)
            if (m_VSync == "on") { QualitySettings.vSyncCount = 1; Application.targetFrameRate = -1; }
            else                 { QualitySettings.vSyncCount = 0; Application.targetFrameRate = -1; }

            Screen.sleepTimeout = SleepTimeout.NeverSleep;
            // 交通/AI 若用了 UnityEngine.Random, 固定种子消掉这一路的轮间差异。
            // (ECS 侧若另有 Random 实例, 这里管不到 —— 属于已知残留风险, 见 README)
            UnityEngine.Random.InitState(20260923);

            SetRenderScale(m_ResScale);

            // 2) 路线: 缺了就响亮地失败, 绝不退回默认相机
            yield return LoadRoute(m_RouteName, r => m_Route = r);
            if (m_Route == null || m_Route.Points.Count < 2)
            {
                Abort(string.IsNullOrEmpty(m_AbortReason) ? "route_invalid" : m_AbortReason);
                yield break;
            }

            // 3) 切到城市场景。上游默认启动的是 Menu, 城市资产在 Main 及其 subscene 里。
            if (SceneManager.GetActiveScene().name != m_UnityScene)
            {
                AsyncOperation load = null;
                try { load = SceneManager.LoadSceneAsync(m_UnityScene, LoadSceneMode.Single); }
                catch (Exception e) { m_AbortReason = "scene_load_failed:" + e.GetType().Name; }
                if (load == null) { Abort("scene_load_failed"); yield break; }
                while (!load.isDone) yield return null;
            }

            // 4) 接管相机
            m_Cam = TakeOverCamera();
            if (m_Cam == null) { Abort("no_camera"); yield break; }

            // 5) 预热: 钉在路线起点渲染 warmup 帧, 等 subscene 流式加载与 shader 落定
            ApplyPose(0f);
            for (int i = 0; i < m_Warmup; i++)
            {
                yield return new WaitForEndOfFrame();
                if (!Application.isFocused) { Abort("focus_lost"); yield break; }
                ApplyPose(0f);
                m_WarmupRendered++;
            }

            // 6) 落 started 标记 —— 采集从这里开始算
            WriteStartedMarker();

            // 7) 被测窗口: 相机位姿只由帧序号决定, 不读 deltaTime。
            //    这是整个载体确定性的根: 同 frames 同 route 两轮走的是逐帧完全相同的位姿序列,
            //    帧率高低不会改变走过的路径, 所以两轮的渲染负载可比。
            for (int i = 0; i < m_Frames; i++)
            {
                yield return new WaitForEndOfFrame();
                if (!Application.isFocused) { Abort("focus_lost"); yield break; }

                float u = (m_Scene == "city_static")
                        ? 0f
                        : (m_Frames <= 1 ? 0f : (float)i / (m_Frames - 1));
                ApplyPose(u);
                m_Rendered++;
            }

            m_CleanExit = true;
            Finish();
        }

        // ---- 相机 --------------------------------------------------------

        Camera TakeOverCamera()
        {
            // 关掉场上所有相机, 换成我们自己的 —— 上游的相机被 ECS/玩家输入驱动,
            // 留着它就没有确定性可言。
            foreach (var c in Camera.allCameras) c.enabled = false;

            var go = new GameObject("[RefbenchCamera]");
            DontDestroyOnLoad(go);
            var cam = go.AddComponent<Camera>();
            cam.fieldOfView = m_Route.Fov > 0f ? m_Route.Fov : 60f;
            cam.nearClipPlane = 0.1f;
            cam.farClipPlane  = m_Route.Far > 0f ? m_Route.Far : 3000f;
            cam.depth = 100;
            // URP 的逐相机附加数据由 URP 在 OnEnable 时补齐, 这里不手动构造,
            // 免得和不同 URP 版本的 UniversalAdditionalCameraData 绑死。
            return cam;
        }

        void ApplyPose(float u)
        {
            m_Route.Sample(u, out var pos, out var rot);
            m_Cam.transform.SetPositionAndRotation(pos, rot);
        }

        static void SetRenderScale(float s)
        {
            // 反射取 URP asset 的 renderScale, 避免对 UniversalRenderPipelineAsset 的编译期依赖
            // (上游 URP 版本变动时这里不会炸编译, 最多是不生效 —— 生效与否回写进 json 由上层核对)
            var rp = GraphicsSettings.currentRenderPipeline;
            if (rp == null) return;
            var p = rp.GetType().GetProperty("renderScale");
            if (p != null && p.CanWrite) p.SetValue(rp, s);
        }

        static float GetRenderScale()
        {
            var rp = GraphicsSettings.currentRenderPipeline;
            var p = rp?.GetType().GetProperty("renderScale");
            return p != null ? Convert.ToSingle(p.GetValue(rp)) : -1f;
        }

        // ---- 路线 --------------------------------------------------------

        sealed class Route
        {
            public string Name = "";
            public string Sha256 = "";
            public float  Fov, Far;
            public readonly List<Vector3> Points = new();
            public readonly List<Quaternion> Rots = new();

            // Catmull-Rom, 端点各自复制一份做哨兵。参数 u 属于 [0,1]。
            public void Sample(float u, out Vector3 pos, out Quaternion rot)
            {
                int n = Points.Count;
                u = Mathf.Clamp01(u);
                float f = u * (n - 1);
                int   i = Mathf.Min((int)f, n - 2);
                float t = f - i;

                Vector3 p0 = Points[Mathf.Max(i - 1, 0)];
                Vector3 p1 = Points[i];
                Vector3 p2 = Points[i + 1];
                Vector3 p3 = Points[Mathf.Min(i + 2, n - 1)];

                pos = 0.5f * ((2f * p1) + (-p0 + p2) * t
                    + (2f * p0 - 5f * p1 + 4f * p2 - p3) * t * t
                    + (-p0 + 3f * p1 - 3f * p2 + p3) * t * t * t);

                rot = Quaternion.Slerp(Rots[i], Rots[i + 1], t);
            }
        }

        IEnumerator LoadRoute(string name, Action<Route> done)
        {
            // 优先读 app files 下 adb push 进来的路线 —— 换路线不用重新出包。
            var pushed = Path.Combine(Application.persistentDataPath, "routes", name + ".json");
            if (File.Exists(pushed))
            {
                byte[] raw = null;
                try { raw = File.ReadAllBytes(pushed); }
                catch (Exception e) { m_AbortReason = "route_invalid:" + e.GetType().Name; }
                done(raw != null ? ParseRoute(name, raw) : null);
                yield break;
            }

            // 退而读 APK 内 StreamingAssets。Android 上它在 jar 里, 只能走 UnityWebRequest。
            var url = Path.Combine(Application.streamingAssetsPath, "refbench_routes", name + ".json");
            using var req = UnityWebRequest.Get(url);
            yield return req.SendWebRequest();
            if (req.result != UnityWebRequest.Result.Success)
            {
                m_AbortReason = "route_missing";
                done(null);
                yield break;
            }
            done(ParseRoute(name, req.downloadHandler.data));
        }

        Route ParseRoute(string name, byte[] raw)
        {
            try
            {
                var dto = JsonUtility.FromJson<RouteDto>(Encoding.UTF8.GetString(raw));
                if (dto?.waypoints == null || dto.waypoints.Length < 2)
                {
                    m_AbortReason = "route_invalid:too_few_waypoints";
                    return null;
                }

                var r = new Route { Name = name, Fov = dto.fov, Far = dto.far };
                foreach (var w in dto.waypoints)
                {
                    r.Points.Add(new Vector3(w.pos[0], w.pos[1], w.pos[2]));
                    r.Rots.Add(Quaternion.Euler(w.rot[0], w.rot[1], w.rot[2]));
                }
                using var sha = SHA256.Create();
                r.Sha256 = ToHex(sha.ComputeHash(raw));
                return r;
            }
            catch (Exception e)
            {
                m_AbortReason = "route_invalid:" + e.GetType().Name;
                return null;
            }
        }

        [Serializable] class RouteDto { public float fov; public float far; public WaypointDto[] waypoints; }
        [Serializable] class WaypointDto { public float[] pos; public float[] rot; }

        // ---- 产出 --------------------------------------------------------

        void WriteStartedMarker()
        {
            var p = Path.Combine(Application.persistentDataPath, StartedFileName);
            WriteAndSync(p, Encoding.UTF8.GetBytes(m_Scene + " " + m_RunId));
        }

        void Abort(string reason)
        {
            m_CleanExit = false;
            m_AbortReason = string.IsNullOrEmpty(m_AbortReason) ? reason : m_AbortReason;
            Finish();
        }

        void Finish()
        {
            try { WriteAndSync(Path.Combine(Application.persistentDataPath, OutputFileName),
                               Encoding.UTF8.GetBytes(BuildJson())); }
            catch (Exception e) { Debug.LogError("[refbench] 输出写盘失败: " + e); }

            Application.Quit();
            // Application.Quit 在 Android 上不是立刻生效, 兜一手确保进程真的走掉,
            // 否则 harness 的存活探测会一直等。
            System.Environment.Exit(m_CleanExit ? 0 : 2);
        }

        static void WriteAndSync(string path, byte[] bytes)
        {
            Directory.CreateDirectory(Path.GetDirectoryName(path));
            using var fs = new FileStream(path, FileMode.Create, FileAccess.Write, FileShare.None);
            fs.Write(bytes, 0, bytes.Length);
            fs.Flush(true);   // true = 落到存储介质, 不只是 OS 缓冲
        }

        string BuildJson()
        {
            var ci = CultureInfo.InvariantCulture;
            var sb = new StringBuilder(2048);
            sb.Append('{');
            J(sb, "contract_version", ContractVersion); sb.Append(',');
            JS(sb, "carrier", Carrier); sb.Append(',');
            JS(sb, "app_version", Application.version); sb.Append(',');
            JS(sb, "harness_version", HarnessVersion); sb.Append(',');
            JS(sb, "upstream_commit", UpstreamCommit.Value); sb.Append(',');
            JS(sb, "run_id", m_RunId); sb.Append(',');
            JS(sb, "scene", m_Scene); sb.Append(',');

            sb.Append("\"params\":{");
            J(sb, "frames", m_Frames); sb.Append(',');
            J(sb, "warmup", m_Warmup); sb.Append(',');
            sb.Append("\"resscale\":").Append(m_ResScale.ToString("R", ci)).Append(',');
            JS(sb, "vsync", m_VSync);
            sb.Append("},");

            sb.Append("\"route\":{");
            JS(sb, "name", m_Route?.Name ?? m_RouteName); sb.Append(',');
            JS(sb, "sha256", m_Route?.Sha256 ?? ""); sb.Append(',');
            J(sb, "waypoints", m_Route?.Points.Count ?? 0);
            sb.Append("},");

            sb.Append("\"graphics\":{");
            JS(sb, "api", SystemInfo.graphicsDeviceType.ToString()); sb.Append(',');
            JS(sb, "device_name", SystemInfo.graphicsDeviceName); sb.Append(',');
            JS(sb, "device_version", SystemInfo.graphicsDeviceVersion); sb.Append(',');
            J(sb, "width", Screen.width); sb.Append(',');
            J(sb, "height", Screen.height); sb.Append(',');
            sb.Append("\"render_scale\":").Append(GetRenderScale().ToString("R", ci)).Append(',');
            J(sb, "vsync_count", QualitySettings.vSyncCount); sb.Append(',');
            J(sb, "target_frame_rate", Application.targetFrameRate); sb.Append(',');
            JS(sb, "quality_level", QualitySettings.names[QualitySettings.GetQualityLevel()]); sb.Append(',');
            JS(sb, "srp_asset", GraphicsSettings.currentRenderPipeline != null
                                ? GraphicsSettings.currentRenderPipeline.name : "none");
            sb.Append("},");

            JS(sb, "render_thread_comm", DetectGfxThreadComm()); sb.Append(',');
            J(sb, "frames_rendered", m_Rendered); sb.Append(',');
            J(sb, "warmup_frames_rendered", m_WarmupRendered); sb.Append(',');
            sb.Append("\"clean_exit\":").Append(m_CleanExit ? "true" : "false");
            if (!m_CleanExit) { sb.Append(','); JS(sb, "abort_reason", m_AbortReason); }
            sb.Append('}');
            return sb.ToString();
        }

        // parse_trace.py --comm 要的就是这个名字。不靠猜, 进程内自己扫一遍。
        static string DetectGfxThreadComm()
        {
            try
            {
                var found = new List<string>();
                foreach (var d in Directory.GetDirectories("/proc/self/task"))
                {
                    var f = Path.Combine(d, "comm");
                    if (!File.Exists(f)) continue;
                    var c = File.ReadAllText(f).Trim();
                    if (c.IndexOf("Gfx", StringComparison.OrdinalIgnoreCase) >= 0) return c;
                    found.Add(c);
                }
                // 没匹配到就把全部线程名交出来供人工定位, 不静默返回一个猜的值
                return "UNRESOLVED[" + string.Join("|", found) + "]";
            }
            catch (Exception e) { return "UNRESOLVED:" + e.GetType().Name; }
        }

        static void J (StringBuilder sb, string k, int v)    => sb.Append('"').Append(k).Append("\":").Append(v);
        static void JS(StringBuilder sb, string k, string v) => sb.Append('"').Append(k).Append("\":\"").Append(Esc(v)).Append('"');
        static string Esc(string s) => (s ?? "").Replace("\\", "\\\\").Replace("\"", "\\\"");
        static string ToHex(byte[] b) { var sb = new StringBuilder(b.Length * 2); foreach (var x in b) sb.Append(x.ToString("x2")); return sb.ToString(); }

        // ---- intent extras ----------------------------------------------

        static string GetIntentExtra(string key)
        {
#if UNITY_ANDROID && !UNITY_EDITOR
            try
            {
                using var up     = new AndroidJavaClass("com.unity3d.player.UnityPlayer");
                using var act    = up.GetStatic<AndroidJavaObject>("currentActivity");
                using var intent = act.Call<AndroidJavaObject>("getIntent");
                var v = intent.Call<string>("getStringExtra", key);
                return string.IsNullOrEmpty(v) ? null : v;
            }
            catch { return null; }
#else
            return null;
#endif
        }

        static int ParseInt(string s, int def, int lo, int hi)
            => int.TryParse(s, NumberStyles.Integer, CultureInfo.InvariantCulture, out var v)
               ? Mathf.Clamp(v, lo, hi) : def;

        static float ParseFloat(string s, float def, float lo, float hi)
            => float.TryParse(s, NumberStyles.Float, CultureInfo.InvariantCulture, out var v)
               ? Mathf.Clamp(v, lo, hi) : def;
    }
}
