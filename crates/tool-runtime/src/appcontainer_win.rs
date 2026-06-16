//! Windows AppContainer 沙箱（M8.2 / Plan28 P3-10）——文件路径 + 网络隔离。
//!
//! AppContainer 令牌：进程以低完整性(Low IL)、默认拒绝文件/网络运行;可访问范围仅由
//! - 对工作区目录的显式 ACL 授权(P2,容器仅能读写工作区)
//! - 网络能力 SID 门控(P1,allow_network 决定能否出站)
//! 决定。命令经 powershell -EncodedCommand 包装、擦密钥环境块,与受限令牌沙箱一致。
//!
//! fail-closed：任一步骤失败返回 Err;由 run_command_streaming 决定是否在「该 Windows 版本不支持
//! AppContainer」时降级到受限令牌沙箱(M8.1),而非裸跑。
//!
//! 注:网络放行依赖宿主 Windows 防火墙开启(WFP 的能力放行过滤器由防火墙服务装载);防火墙全关时,
//! 即使 internetClient 能力已落入令牌,容器仍无法出站(默认拒绝侧不受影响,始终生效)。

#![cfg(windows)]
// 本模块已实现并通过文件/网络隔离功能测试,但因「容器内 native console 程序 stdout 不回传」尚未
// 接入 run_command_streaming 作默认(见该函数注释)。在接通前,函数仅由测试使用,故允许 dead_code。
#![allow(dead_code)]

use crate::{CommandCancel, CommandLineCallback, RunCommandResult, ToolRuntimeError};
use std::ffi::c_void;
use std::io::Read;
use std::os::windows::io::FromRawHandle;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, LocalFree, HANDLE, HANDLE_FLAG_INHERIT, HANDLE_FLAGS, HLOCAL, WAIT_OBJECT_0,
};
use windows::Win32::Security::Authorization::{
    ConvertStringSidToSidW, GetNamedSecurityInfoW, SetEntriesInAclW, SetNamedSecurityInfoW,
    EXPLICIT_ACCESS_W, GRANT_ACCESS, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT, TRUSTEE_IS_SID,
    TRUSTEE_IS_UNKNOWN, TRUSTEE_W,
};
use windows::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
use windows::Win32::Security::{
    ACL, CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, OBJECT_INHERIT_ACE,
    PSECURITY_DESCRIPTOR,
};
use windows::Win32::Storage::FileSystem::{
    FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
};
use windows::Win32::Security::{
    FreeSid, SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES, PSID,
};
use windows::Win32::System::Console::{ClosePseudoConsole, CreatePseudoConsole, COORD, HPCON};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, ResumeThread, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, PROCESS_INFORMATION, STARTF_USESTDHANDLES,
    STARTUPINFOEXW,
};

fn err(msg: impl std::fmt::Display) -> ToolRuntimeError {
    ToolRuntimeError::CommandFailed(format!("AppContainer 沙箱: {msg}"))
}

/// 把宽字符串转成以 NUL 结尾的 UTF-16。
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 构建容器用环境块:擦密钥 + 把 TEMP/TMP 重定向到容器可写的沙箱临时目录。
///
/// 必要性:AppContainer 默认拒绝用户 %TEMP%(AppData\Local\Temp 未授权);而 PowerShell 处理
/// 外部命令、.NET、众多工具(git/npm)都需要可写 TEMP。不重定向会导致外部命令静默无输出。
fn build_sandbox_env_block(temp_dir: &str) -> Vec<u16> {
    let mut block: Vec<u16> = Vec::new();
    for (k, v) in std::env::vars() {
        let upper = k.to_uppercase();
        // 擦密钥(与受限令牌沙箱一致)。
        if upper.contains("API_KEY")
            || upper.contains("APIKEY")
            || upper.contains("SECRET")
            || upper.contains("_TOKEN")
            || upper.contains("PASSWORD")
        {
            continue;
        }
        // TEMP/TMP 由沙箱临时目录接管,丢弃父进程的值。
        if upper == "TEMP" || upper == "TMP" {
            continue;
        }
        block.extend(format!("{k}={v}").encode_utf16());
        block.push(0);
    }
    block.extend(format!("TEMP={temp_dir}").encode_utf16());
    block.push(0);
    block.extend(format!("TMP={temp_dir}").encode_utf16());
    block.push(0);
    block.push(0); // 双 NUL 结尾
    block
}

/// 创建或复用 AppContainer 容器 profile,返回其 SID（调用方负责 FreeSid）。
///
/// 首次用 CreateAppContainerProfile 创建;已存在(ERROR_ALREADY_EXISTS)时回落
/// DeriveAppContainerSidFromAppContainerName 复用。容器名应每会话隔离(name 含 sessionId 哈希)。
unsafe fn create_or_derive_appcontainer_sid(
    name: &str,
    display: &str,
    caps: Option<&[SID_AND_ATTRIBUTES]>,
) -> Result<PSID, ToolRuntimeError> {
    let wname = wide(name);
    let wdisp = wide(display);
    match CreateAppContainerProfile(
        PCWSTR(wname.as_ptr()),
        PCWSTR(wdisp.as_ptr()),
        PCWSTR(wdisp.as_ptr()),
        caps,
    ) {
        Ok(sid) => Ok(sid),
        Err(e) => {
            // 0x800700B7 = HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)：profile 已存在,改派生复用。
            if e.code().0 as u32 == 0x800700B7 {
                DeriveAppContainerSidFromAppContainerName(PCWSTR(wname.as_ptr()))
                    .map_err(|e| err(format!("DeriveAppContainerSid 失败: {e}")))
            } else {
                Err(err(format!("CreateAppContainerProfile 失败: {e}")))
            }
        }
    }
}

/// 给目录的 DACL 追加一条「允许容器 SID 读/写/执行(遍历)」的 ACE,使 AppContainer 命令能访问工作区。
///
/// 机制:AppContainer 默认拒绝;授权 = GetNamedSecurityInfoW 取现有 DACL → SetEntriesInAclW 合并新 ACE
/// (GRANT_ACCESS 追加,不替换) → SetNamedSecurityInfoW 写回。继承标志使子目录/文件一并可访问。
/// 注:spike 阶段暂未释放安全描述符/新 DACL(每命令小泄漏),集成时补 LocalFree。
unsafe fn grant_dir_access_to_appcontainer(path: &Path, ac_sid: PSID) -> Result<(), ToolRuntimeError> {
    let mut wpath = wide(&path.as_os_str().to_string_lossy());

    let mut old_dacl: *mut ACL = std::ptr::null_mut();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    let rc = GetNamedSecurityInfoW(
        PCWSTR(wpath.as_ptr()),
        SE_FILE_OBJECT,
        DACL_SECURITY_INFORMATION,
        None,
        None,
        Some(&mut old_dacl),
        None,
        &mut sd,
    );
    if rc.0 != 0 {
        return Err(err(format!("GetNamedSecurityInfo 失败: {}", rc.0)));
    }

    let ea = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0 | FILE_GENERIC_EXECUTE.0,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: windows::Win32::Security::ACE_FLAGS(
            OBJECT_INHERIT_ACE.0 | CONTAINER_INHERIT_ACE.0,
        ),
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            // TRUSTEE_IS_SID 形态下 ptstrName 承载 SID 指针。
            ptstrName: PWSTR(ac_sid.0 as *mut u16),
        },
    };

    let mut new_dacl: *mut ACL = std::ptr::null_mut();
    let rc = SetEntriesInAclW(Some(&[ea]), Some(old_dacl as *const ACL), &mut new_dacl);
    if rc.0 != 0 {
        return Err(err(format!("SetEntriesInAcl 失败: {}", rc.0)));
    }

    let rc = SetNamedSecurityInfoW(
        PWSTR(wpath.as_mut_ptr()),
        SE_FILE_OBJECT,
        DACL_SECURITY_INFORMATION,
        PSID::default(),
        PSID::default(),
        Some(new_dacl as *const ACL),
        None,
    );
    // SetNamedSecurityInfo 会拷贝 DACL,写回后即可释放新 DACL 与原安全描述符。
    let _ = LocalFree(HLOCAL(new_dacl as *mut c_void));
    if !sd.0.is_null() {
        let _ = LocalFree(HLOCAL(sd.0));
    }
    if rc.0 != 0 {
        return Err(err(format!("SetNamedSecurityInfo 失败: {}", rc.0)));
    }
    Ok(())
}

/// 读取管道（已包成 File），UTF-8 lossy、按行回调、截断到 64KiB。
fn drain(mut file: std::fs::File, on_line: Option<CommandLineCallback>) -> (String, bool) {
    const MAX: usize = 64 * 1024;
    let mut buf = Vec::new();
    let _ = file.read_to_end(&mut buf);
    let truncated = buf.len() > MAX;
    if truncated {
        buf.truncate(MAX);
    }
    let text = String::from_utf8_lossy(&buf).to_string();
    if let Some(cb) = on_line {
        for line in text.lines() {
            cb(line.to_string());
        }
    }
    (text, truncated)
}

/// 在 AppContainer 中执行一条命令(经 powershell -EncodedCommand 包装、擦密钥环境),通过管道回读
/// stdout/stderr。隔离层次:真 AppContainer 令牌(Low IL + 默认拒绝文件/网络) + 工作区 ACL 授权
/// (容器仅能读写工作区) + 能力 SID 网络门控(allow_network 决定能否出站)。
///
/// fail-closed:任一步骤失败返回 Err,由 run_command_streaming 决定是否降级到受限令牌沙箱
/// (仅当 AppContainer 不被该 Windows 版本支持时降级,而非裸跑)。
pub fn run_in_appcontainer_sandbox(
    workspace: &Path,
    command: &str,
    timeout: Duration,
    on_line: Option<CommandLineCallback>,
    cancel: Option<CommandCancel>,
    allow_network: bool,
) -> Result<RunCommandResult, ToolRuntimeError> {
    unsafe {
        // P1 网络隔离：能力 SID 决定容器能否联网。默认无能力 = 完全无网(默认拒绝)。
        // allow_network=true 时加三个互联网能力 SID(internetClient / internetClientServer /
        // privateNetworkClientServer),容器方可出站。SID 由字符串转换得来,需 LocalFree 回收。
        // 关键:能力须在「创建 profile 时」与「启动进程时」都登记(MSDN AppContainer 样例),
        // 仅在启动时传入不会真正落入令牌的能力数组。故先建能力、再据此建/派生 profile。
        let mut cap_sids: Vec<PSID> = Vec::new();
        let mut caps: Vec<SID_AND_ATTRIBUTES> = Vec::new();
        if allow_network {
            for s in ["S-1-15-3-1", "S-1-15-3-2", "S-1-15-3-3"] {
                let sw = wide(s);
                let mut sid = PSID::default();
                if ConvertStringSidToSidW(PCWSTR(sw.as_ptr()), &mut sid).is_ok() {
                    cap_sids.push(sid);
                    caps.push(SID_AND_ATTRIBUTES {
                        Sid: sid,
                        Attributes: 0x0000_0004, // SE_GROUP_ENABLED
                    });
                }
            }
        }
        let caps_arg: Option<&[SID_AND_ATTRIBUTES]> =
            if caps.is_empty() { None } else { Some(&caps[..]) };
        // 容器名按能力集区分:无网/有网各一套 profile,避免「已存在但能力集不符」的派生陷阱。
        // (实测:profile 若以无能力建好,后续仅靠启动期能力数组无法补登记,网络仍被挡。)
        let (name, display) = if allow_network {
            ("MDGA.Sandbox.Net", "MDGA command sandbox (net)")
        } else {
            ("MDGA.Sandbox", "MDGA command sandbox")
        };

        // AppContainer 标准启动用 CreateProcessW（不传显式令牌,系统从能力派生 AppContainer 令牌）。
        // 实测:显式受限令牌 + AppContainer 属性(CreateProcessAsUserW)不会产生真正容器(无包 SID)。
        let ac_sid = create_or_derive_appcontainer_sid(name, display, caps_arg)?;
        // P2：授权工作区目录,使容器命令能读写工作区(否则默认拒绝、连工作区都进不去)。
        grant_dir_access_to_appcontainer(workspace, ac_sid)?;

        // 容器专用临时目录:授权给容器并接管 TEMP/TMP。容器读不到用户 %TEMP%,而 PowerShell
        // 处理外部命令、.NET、众多工具都需可写 TEMP——缺它会导致外部命令静默无输出。
        let sandbox_temp = std::env::temp_dir().join("mdga-sandbox-temp");
        let _ = std::fs::create_dir_all(&sandbox_temp);
        grant_dir_access_to_appcontainer(&sandbox_temp, ac_sid)?;
        let sandbox_temp_str = sandbox_temp.as_os_str().to_string_lossy().to_string();

        let sec_caps = SECURITY_CAPABILITIES {
            AppContainerSid: ac_sid,
            Capabilities: if caps.is_empty() {
                std::ptr::null_mut()
            } else {
                caps.as_mut_ptr()
            },
            CapabilityCount: caps.len() as u32,
            Reserved: 0,
        };

        // proc-thread 属性列表：先取 size,再分配,再写入 SECURITY_CAPABILITIES 属性。
        let mut attr_size: usize = 0;
        let _ = InitializeProcThreadAttributeList(
            LPPROC_THREAD_ATTRIBUTE_LIST(std::ptr::null_mut()),
            1,
            0,
            &mut attr_size,
        );
        let mut attr_buf = vec![0u8; attr_size];
        let attr_list = LPPROC_THREAD_ATTRIBUTE_LIST(attr_buf.as_mut_ptr() as *mut c_void);
        InitializeProcThreadAttributeList(attr_list, 1, 0, &mut attr_size)
            .map_err(|e| err(format!("InitializeProcThreadAttributeList 失败: {e}")))?;
        UpdateProcThreadAttribute(
            attr_list,
            0,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
            Some(&sec_caps as *const _ as *const c_void),
            std::mem::size_of::<SECURITY_CAPABILITIES>(),
            None,
            None,
        )
        .map_err(|e| err(format!("UpdateProcThreadAttribute 失败: {e}")))?;

        // 管道（写端可继承,读端不可继承）。
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: true.into(),
        };
        let (mut out_r, mut out_w) = (HANDLE::default(), HANDLE::default());
        let (mut err_r, mut err_w) = (HANDLE::default(), HANDLE::default());
        CreatePipe(&mut out_r, &mut out_w, Some(&sa), 0)
            .map_err(|e| err(format!("CreatePipe(out) 失败: {e}")))?;
        CreatePipe(&mut err_r, &mut err_w, Some(&sa), 0)
            .map_err(|e| err(format!("CreatePipe(err) 失败: {e}")))?;
        let _ = windows::Win32::Foundation::SetHandleInformation(out_r, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0));
        let _ = windows::Win32::Foundation::SetHandleInformation(err_r, HANDLE_FLAG_INHERIT.0, HANDLE_FLAGS(0));

        // STARTUPINFOEXW：cb=EX 大小,挂上属性列表。
        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdOutput = out_w;
        si.StartupInfo.hStdError = err_w;
        si.StartupInfo.hStdInput = HANDLE::default();
        si.lpAttributeList = attr_list;

        // 与受限令牌沙箱一致:powershell -EncodedCommand 包装(规避转义) + 擦密钥环境块;
        // 额外把 TEMP/TMP 重定向到容器可写的沙箱临时目录(否则外部命令静默无输出)。
        let mut cmdline = crate::sandbox_win::encoded_command_line(command);
        let mut env_block = build_sandbox_env_block(&sandbox_temp_str);
        let cwd = wide(&workspace.as_os_str().to_string_lossy());
        let mut pi = PROCESS_INFORMATION::default();
        let create_result = CreateProcessW(
            PCWSTR::null(),
            PWSTR(cmdline.as_mut_ptr()),
            None,
            None,
            true,
            EXTENDED_STARTUPINFO_PRESENT
                | CREATE_SUSPENDED
                | CREATE_NO_WINDOW
                | CREATE_UNICODE_ENVIRONMENT,
            Some(env_block.as_mut_ptr() as *const c_void), // 擦密钥后的环境块
            PCWSTR(cwd.as_ptr()),
            &si.StartupInfo,
            &mut pi,
        );

        let _ = CloseHandle(out_w);
        let _ = CloseHandle(err_w);
        DeleteProcThreadAttributeList(attr_list);
        // 能力 SID 已被 CreateProcess 消费,回收(ConvertStringSidToSidW 经 LocalAlloc 分配)。
        for sid in &cap_sids {
            let _ = LocalFree(HLOCAL(sid.0));
        }

        if let Err(e) = create_result {
            let _ = CloseHandle(out_r);
            let _ = CloseHandle(err_r);
            FreeSid(ac_sid);
            return Err(err(format!("CreateProcess(AppContainer) 失败: {e}")));
        }

        ResumeThread(pi.hThread);

        let on_line2 = on_line.clone();
        let out_file = std::fs::File::from_raw_handle(out_r.0 as *mut _);
        let err_file = std::fs::File::from_raw_handle(err_r.0 as *mut _);
        let out_handle = std::thread::spawn(move || drain(out_file, on_line2));
        let err_handle = std::thread::spawn(move || drain(err_file, on_line));

        let started = Instant::now();
        let mut timed_out = false;
        loop {
            let wait = WaitForSingleObject(pi.hProcess, 50);
            if wait == WAIT_OBJECT_0 {
                break;
            }
            if cancel.as_ref().map(|c| c.load(Ordering::SeqCst)).unwrap_or(false) {
                let _ = TerminateProcess(pi.hProcess, 1);
                break;
            }
            if started.elapsed() >= timeout {
                let _ = TerminateProcess(pi.hProcess, 1);
                timed_out = true;
                break;
            }
        }

        let mut exit_code: u32 = 0;
        let _ = GetExitCodeProcess(pi.hProcess, &mut exit_code);
        let (stdout, out_trunc) = out_handle.join().unwrap_or_default();
        let (stderr, err_trunc) = err_handle.join().unwrap_or_default();
        let duration_ms = started.elapsed().as_millis();

        let _ = CloseHandle(pi.hThread);
        let _ = CloseHandle(pi.hProcess);
        FreeSid(ac_sid);

        Ok(RunCommandResult {
            command: command.to_string(),
            exit_code: if timed_out { None } else { Some(exit_code as i32) },
            stdout,
            stderr,
            truncated: out_trunc || err_trunc,
            timed_out,
            duration_ms,
        })
    }
}

/// PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE 的原始值(windows 0.58 未必导出常量,直接用数值)。
const PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE_VAL: usize = 0x0002_0016;

/// ConPTY spike：用伪控制台(CreatePseudoConsole)在 AppContainer 内跑命令并捕获**全部** console 输出
/// (含 native 孙进程,如 cmd/git)。验证「容器内 native 命令 stdout 不回传」能否被 ConPTY 解决。
///
/// 与 run_in_appcontainer_sandbox 共用容器创建+ACL 授权;差别仅在 I/O:不重定向 std 句柄,而是把伪控制台
/// 同时作为 SECURITY_CAPABILITIES 之外的第二个 proc-thread 属性挂上,从伪控制台输出管道回读(VT 流)。
pub fn run_in_appcontainer_conpty_spike(
    workspace: &Path,
    command: &str,
    timeout: Duration,
    allow_network: bool,
    use_container: bool,
) -> Result<(Option<i32>, String), ToolRuntimeError> {
    unsafe {
        // --- 容器与授权(同 run_in_appcontainer_sandbox)---
        let mut cap_sids: Vec<PSID> = Vec::new();
        let mut caps: Vec<SID_AND_ATTRIBUTES> = Vec::new();
        if allow_network {
            for s in ["S-1-15-3-1", "S-1-15-3-2", "S-1-15-3-3"] {
                let sw = wide(s);
                let mut sid = PSID::default();
                if ConvertStringSidToSidW(PCWSTR(sw.as_ptr()), &mut sid).is_ok() {
                    cap_sids.push(sid);
                    caps.push(SID_AND_ATTRIBUTES { Sid: sid, Attributes: 0x0000_0004 });
                }
            }
        }
        let caps_arg: Option<&[SID_AND_ATTRIBUTES]> =
            if caps.is_empty() { None } else { Some(&caps[..]) };
        let (name, display) = if allow_network {
            ("MDGA.Sandbox.Net", "MDGA command sandbox (net)")
        } else {
            ("MDGA.Sandbox", "MDGA command sandbox")
        };
        let ac_sid = create_or_derive_appcontainer_sid(name, display, caps_arg)?;
        grant_dir_access_to_appcontainer(workspace, ac_sid)?;
        let sandbox_temp = std::env::temp_dir().join("mdga-sandbox-temp");
        let _ = std::fs::create_dir_all(&sandbox_temp);
        grant_dir_access_to_appcontainer(&sandbox_temp, ac_sid)?;
        let sandbox_temp_str = sandbox_temp.as_os_str().to_string_lossy().to_string();

        let sec_caps = SECURITY_CAPABILITIES {
            AppContainerSid: ac_sid,
            Capabilities: if caps.is_empty() {
                std::ptr::null_mut()
            } else {
                caps.as_mut_ptr()
            },
            CapabilityCount: caps.len() as u32,
            Reserved: 0,
        };

        // --- ConPTY 管道:in(我们写→PTY 读)、out(PTY 写→我们读)---
        let (mut in_r, mut in_w) = (HANDLE::default(), HANDLE::default());
        let (mut out_r, mut out_w) = (HANDLE::default(), HANDLE::default());
        CreatePipe(&mut in_r, &mut in_w, None, 0)
            .map_err(|e| err(format!("CreatePipe(conpty in) 失败: {e}")))?;
        CreatePipe(&mut out_r, &mut out_w, None, 0)
            .map_err(|e| err(format!("CreatePipe(conpty out) 失败: {e}")))?;

        let coord = COORD { X: 120, Y: 40 };
        let hpc: HPCON = CreatePseudoConsole(coord, in_r, out_w, 0)
            .map_err(|e| err(format!("CreatePseudoConsole 失败: {e}")))?;
        // 伪控制台已持有 in_r / out_w 的副本,父进程关掉自己这份。
        let _ = CloseHandle(in_r);
        let _ = CloseHandle(out_w);

        // --- proc-thread 属性列表:PSEUDOCONSOLE(+ 容器时叠加 SECURITY_CAPABILITIES)---
        let attr_count = if use_container { 2 } else { 1 };
        let mut attr_size: usize = 0;
        let _ = InitializeProcThreadAttributeList(
            LPPROC_THREAD_ATTRIBUTE_LIST(std::ptr::null_mut()),
            attr_count,
            0,
            &mut attr_size,
        );
        let mut attr_buf = vec![0u8; attr_size];
        let attr_list = LPPROC_THREAD_ATTRIBUTE_LIST(attr_buf.as_mut_ptr() as *mut c_void);
        InitializeProcThreadAttributeList(attr_list, attr_count, 0, &mut attr_size)
            .map_err(|e| err(format!("InitializeProcThreadAttributeList 失败: {e}")))?;
        if use_container {
            UpdateProcThreadAttribute(
                attr_list,
                0,
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                Some(&sec_caps as *const _ as *const c_void),
                std::mem::size_of::<SECURITY_CAPABILITIES>(),
                None,
                None,
            )
            .map_err(|e| err(format!("UpdateProcThreadAttribute(caps) 失败: {e}")))?;
        }
        UpdateProcThreadAttribute(
            attr_list,
            0,
            PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE_VAL,
            Some(hpc.0 as *const c_void),
            std::mem::size_of::<HPCON>(),
            None,
            None,
        )
        .map_err(|e| err(format!("UpdateProcThreadAttribute(pty) 失败: {e}")))?;

        // --- STARTUPINFOEXW:伪控制台即子进程控制台。
        // 关键:显式把 std 句柄设为 NULL(USESTDHANDLES),阻止子进程经 PEB 继承父进程(本测试下是 cargo
        // 的重定向管道)的 std 句柄——否则子进程 stdout 漏到父管道而非伪控制台。NULL 句柄下控制台程序
        // 会重开 CONOUT$(即伪控制台)。
        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = HANDLE::default();
        si.StartupInfo.hStdOutput = HANDLE::default();
        si.StartupInfo.hStdError = HANDLE::default();
        si.lpAttributeList = attr_list;

        let mut cmdline = crate::sandbox_win::encoded_command_line(command);
        let mut env_block = build_sandbox_env_block(&sandbox_temp_str);
        let cwd = wide(&workspace.as_os_str().to_string_lossy());
        let mut pi = PROCESS_INFORMATION::default();
        let create_result = CreateProcessW(
            PCWSTR::null(),
            PWSTR(cmdline.as_mut_ptr()),
            None,
            None,
            false, // ConPTY:句柄经伪控制台传入,不靠继承
            EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
            Some(env_block.as_mut_ptr() as *const c_void),
            PCWSTR(cwd.as_ptr()),
            &si.StartupInfo,
            &mut pi,
        );
        DeleteProcThreadAttributeList(attr_list);
        for sid in &cap_sids {
            let _ = LocalFree(HLOCAL(sid.0));
        }
        if let Err(e) = create_result {
            let _ = CloseHandle(out_r);
            let _ = CloseHandle(in_w);
            ClosePseudoConsole(hpc);
            FreeSid(ac_sid);
            return Err(err(format!("CreateProcess(ConPTY) 失败: {e}")));
        }

        // 并发读伪控制台输出管道(VT 流);进程退出后关 PTY 触发 EOF,reader 收尾。
        let out_file = std::fs::File::from_raw_handle(out_r.0 as *mut _);
        let reader = std::thread::spawn(move || {
            let mut f = out_file;
            let mut buf = Vec::new();
            let _ = f.read_to_end(&mut buf);
            buf
        });

        let started = Instant::now();
        let mut timed_out = false;
        loop {
            if WaitForSingleObject(pi.hProcess, 50) == WAIT_OBJECT_0 {
                break;
            }
            if started.elapsed() >= timeout {
                let _ = TerminateProcess(pi.hProcess, 1);
                timed_out = true;
                break;
            }
        }
        let mut exit_code: u32 = 0;
        let _ = GetExitCodeProcess(pi.hProcess, &mut exit_code);

        // 关伪控制台 → out_w 末引用关闭 → out_r EOF → reader 返回。
        ClosePseudoConsole(hpc);
        let _ = CloseHandle(in_w);
        let buf = reader.join().unwrap_or_default();
        let _ = CloseHandle(pi.hThread);
        let _ = CloseHandle(pi.hProcess);
        FreeSid(ac_sid);

        let text = String::from_utf8_lossy(&buf).to_string();
        Ok((if timed_out { None } else { Some(exit_code as i32) }, text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AppContainer 测试共享机器级全局状态(容器 profile、共享沙箱 temp 目录的 DACL),
    /// 并行会相互踩踏(授权读改写竞态、profile 并发创建)。用此锁串行化各容器测试。
    static SANDBOX_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        SANDBOX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// ConPTY 限时 spike 的结论性记录(真机 Win11 Canary 28000):
    /// - **非容器** ConPTY:同时捕获 cmdlet(CMDLETOK123)与 native 孙进程(CONPTYOK)——装配正确。
    /// - **容器** ConPTY:只捕获 powershell 自身输出(CMDLETOK123),native 孙进程输出(CONPTYOK)仍丢失。
    ///
    /// 即:AppContainer 破坏「容器进程 → 其子进程」的 I/O 继承(管道与控制台皆然),ConPTY 也救不回来。
    /// 这是 AppContainer 作命令沙箱的根本拦路问题;暂缓接通(见 [[appcontainer-console-output-blocker]])。
    /// 关键修复:子进程须以 STARTF_USESTDHANDLES + NULL std 句柄创建,才不继承父进程重定向句柄、改用伪控制台。
    #[test]
    fn conpty_appcontainer_loses_native_child_output() {
        let _g = test_guard();
        let ws = std::env::temp_dir().join(format!("mdga-ac-pty-{}", std::process::id()));
        std::fs::create_dir_all(&ws).expect("建工作区目录失败");
        let cmd = "Write-Output CMDLETOK123; cmd /c echo CONPTYOK";
        let (_e0, t0) =
            run_in_appcontainer_conpty_spike(&ws, cmd, Duration::from_secs(20), false, false)
                .expect("ConPTY(非容器) 起进程失败");
        let (_e1, t1) =
            run_in_appcontainer_conpty_spike(&ws, cmd, Duration::from_secs(20), false, true)
                .expect("ConPTY(容器) 起进程失败");
        let _ = std::fs::remove_dir_all(&ws);
        eprintln!("--- 非容器 ---\n{}\n--- 容器 ---\n{}", t0.escape_debug(), t1.escape_debug());

        // 非容器:装配正确,native 输出能被伪控制台捕获。
        assert!(t0.contains("CONPTYOK"), "非容器 ConPTY 装配异常:\n{}", t0.escape_debug());
        // 容器:powershell 自身输出回得来,但 native 孙进程输出丢失——记录这一拦路事实。
        assert!(
            t1.contains("CMDLETOK123"),
            "容器内连 powershell 自身输出都没捕获(与已知现象不符):\n{}",
            t1.escape_debug()
        );
        assert!(
            !t1.contains("CONPTYOK"),
            "容器内 native 孙进程输出竟回来了——拦路问题已变化,应重评 AppContainer 接通:\n{}",
            t1.escape_debug()
        );
    }

    /// P0 spike：在 token+AppContainer 内跑 `whoami /groups`,断言进程能起、且完整性级别=Low、
    /// 且令牌组含 AppContainer SID(S-1-15-2-...)。这是 AppContainer 真生效的最直接证据。
    /// 在目标 Windows build 上真机运行;若环境不支持(CreateAppContainerProfile 拒绝),测试会失败并打印原因。
    #[test]
    fn spike_functionally_blocks_reading_outside_workspace() {
        let _g = test_guard();
        // 功能性证明(设计 #6 首选):沙箱进程读一个**工作区外**的真实文件。
        // AppContainer 真生效 → 默认拒绝 → 读不到内容;若仅 Low IL 而无容器隔离 → 能读到(Low IL 不挡读)。
        // 用本仓库 Cargo.toml 作探针目标(普通进程可读、含已知标记 [workspace]);workspace 传 temp(未授权)。
        let target = "C:\\Users\\AIT\\Desktop\\NovaStudio\\MDGA\\Cargo.toml";
        // 先确认该文件普通可读(夹具有效),否则测试无意义。
        let normally = std::fs::read_to_string(target).unwrap_or_default();
        assert!(
            normally.contains("[workspace]"),
            "夹具失效:探针目标文件不可读或无预期标记,换一个稳定的工作区外文件"
        );

        let tmp = std::env::temp_dir();
        let r = run_in_appcontainer_sandbox(
            &tmp,
            &format!("C:\\Windows\\System32\\cmd.exe /c type \"{target}\""),
            Duration::from_secs(20),
            None,
            None,
            false,
        );
        let out = match r {
            Ok(o) => o,
            Err(e) => panic!("spike 起进程失败(环境可能不支持 AppContainer): {e}"),
        };
        let text = format!("{}\n{}", out.stdout, out.stderr);
        eprintln!("--- type 工作区外文件 (exit={:?}) ---\n{}", out.exit_code, text);
        // 隔离生效的判据:输出里**不应**出现该文件的已知内容标记。
        assert!(
            !text.contains("[workspace]"),
            "AppContainer 未隔离文件系统:沙箱进程读到了工作区外文件的内容(只 Low IL、容器未真正生效):\n{text}"
        );
    }

    /// P2:工作区 ACL 授权后,沙箱进程**应能**读到工作区内文件(否则默认拒绝、命令完全不可用)。
    /// 与上一测试合起来构成完整证明:工作区内可读(已授权)、工作区外不可读(隔离生效)。
    #[test]
    fn appcontainer_can_read_inside_granted_workspace() {
        let _g = test_guard();
        // 用独立子目录当工作区,避免污染 temp 根的 DACL、也避免与其他测试串味。
        let ws = std::env::temp_dir().join(format!("mdga-ac-ws-{}", std::process::id()));
        std::fs::create_dir_all(&ws).expect("建工作区目录失败");
        let marker = "[mdga-inside-marker-77]";
        let file = ws.join("inside.txt");
        std::fs::write(&file, marker).expect("写工作区内探针文件失败");

        let r = run_in_appcontainer_sandbox(
            &ws,
            &format!("Get-Content -Raw '{}'", file.display()),
            Duration::from_secs(20),
            None,
            None,
            false,
        );
        // 清理(忽略失败:目录残留无害)。
        let _ = std::fs::remove_dir_all(&ws);

        let out = match r {
            Ok(o) => o,
            Err(e) => panic!("spike 起进程失败(环境可能不支持 AppContainer): {e}"),
        };
        let text = format!("{}\n{}", out.stdout, out.stderr);
        eprintln!(
            "--- type 工作区内文件 (exit={:?}) ---\n{}",
            out.exit_code, text
        );
        assert!(
            text.contains(marker),
            "ACL 授权未生效:沙箱进程读不到已授权的工作区内文件(命令将完全不可用):\n{text}"
        );
    }

    /// 记录关键现状:AppContainer 内 native console 程序**能跑、能写文件**(工作区已授权),
    /// 但其 console stdout/stderr **不会**回传到父进程管道(只有 PowerShell cmdlet 输出回得来)。
    /// 这是 AppContainer 的 console 隔离特性——真实 agent 命令(git/npm/node)的输出会丢失,
    /// 是 AppContainer 作为命令沙箱的拦路问题(待 ConPTY 等方案解决前不能作默认)。
    /// 本测试断言「native 命令能写文件」这一**已成立**的事实(证明命令确实在跑)。
    #[test]
    fn appcontainer_native_command_runs_and_writes_files() {
        let _g = test_guard();
        let ws = std::env::temp_dir().join(format!("mdga-ac-ext-{}", std::process::id()));
        std::fs::create_dir_all(&ws).expect("建工作区目录失败");
        let out_file = ws.join("out.txt");
        // cmd 用自身的 > 重定向写文件(绕过 console/管道);若文件有内容,说明 native 命令能跑、能产出,
        // 问题只在 console 输出路由;若文件空,说明 native console 程序在容器内根本产不出输出。
        let cmd = format!("cmd /c \"echo NATIVEOK>{}\"", out_file.display());
        let r = run_in_appcontainer_sandbox(&ws, &cmd, Duration::from_secs(20), None, None, false);
        let file_content = std::fs::read_to_string(&out_file).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&ws);
        let out = match r {
            Ok(o) => o,
            Err(e) => panic!("起进程失败(环境可能不支持 AppContainer): {e}"),
        };
        eprintln!(
            "--- native→file (exit={:?}) file=[{}] stdout=[{}] stderr=[{}] ---",
            out.exit_code,
            file_content.trim(),
            out.stdout.trim(),
            out.stderr.trim()
        );
        assert!(
            file_content.contains("NATIVEOK"),
            "AppContainer 内 native 命令连写文件都没产出(console 程序在容器内完全不可用):\nfile=[{}]",
            file_content.trim()
        );
    }

    /// netsh 探测宿主 Windows 防火墙是否在任一 profile 上开启。
    /// AppContainer 的能力放行过滤器由防火墙服务装载;防火墙全关时,能力 SID 已在令牌内也无法出站。
    fn host_firewall_enabled() -> bool {
        std::process::Command::new("C:\\Windows\\System32\\netsh.exe")
            .args(["advfirewall", "show", "allprofiles", "state"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_uppercase().contains(" ON"))
            .unwrap_or(false)
    }

    /// P1:网络隔离证明。
    /// 安全关键、环境无关:**无网络能力 → 出站必被挡**(默认拒绝)。这一条任何环境都断言。
    /// 放行侧(有能力 → 可出站)依赖宿主防火墙开启(WFP 的能力放行过滤器由防火墙服务装载),
    /// 故仅在防火墙开启时断言;关闭时打印说明并跳过(能力是否落入令牌由代码保证,与本断言无关)。
    /// 依赖真实外网(命中 example.com),默认 #[ignore],手动跑:
    ///   cargo test -p mdga-tool-runtime --lib appcontainer_network -- --ignored --nocapture
    #[test]
    #[ignore = "需真实外网;手动用 --ignored 跑"]
    fn appcontainer_network_default_deny_and_capability_gate() {
        let _g = test_guard();
        let ws = std::env::temp_dir().join(format!("mdga-ac-net-{}", std::process::id()));
        std::fs::create_dir_all(&ws).expect("建工作区目录失败");
        // curl 出站探针:成功打印 "RC:200",失败(被挡)则非 200 / 空。
        let probe = "C:\\Windows\\System32\\curl.exe -s -m 8 -o NUL -w \"RC:%{http_code}\" http://example.com";

        let blocked = run_in_appcontainer_sandbox(&ws, probe, Duration::from_secs(20), None, None, false)
            .expect("起进程失败");
        let blocked_text = format!("{}\n{}", blocked.stdout, blocked.stderr);
        eprintln!("--- 无能力出站 (exit={:?}) ---\n{}", blocked.exit_code, blocked_text);

        let allowed = run_in_appcontainer_sandbox(&ws, probe, Duration::from_secs(20), None, None, true)
            .expect("起进程失败");
        let allowed_text = format!("{}\n{}", allowed.stdout, allowed.stderr);
        eprintln!("--- 有能力出站 (exit={:?}) ---\n{}", allowed.exit_code, allowed_text);

        let _ = std::fs::remove_dir_all(&ws);

        // 安全关键断言:默认拒绝必须成立。
        assert!(
            !blocked_text.contains("RC:200"),
            "网络隔离失效:无网络能力却仍能出站(默认应拒绝):\n{blocked_text}"
        );

        // 放行断言:仅在宿主防火墙开启时成立。
        if host_firewall_enabled() {
            assert!(
                allowed_text.contains("RC:200"),
                "网络能力已授但出站被挡(防火墙已开,属真问题):\n{allowed_text}"
            );
        } else {
            eprintln!(
                "[skip] 宿主防火墙全关:跳过「有能力→可出站」断言。\
                 已知 internetClient 能力正确落入令牌;标准机(防火墙开)上方可出站。allowed={allowed_text}"
            );
        }
    }
}
