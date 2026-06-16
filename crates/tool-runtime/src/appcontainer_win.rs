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

use crate::{CommandCancel, CommandLineCallback, RunCommandResult, ToolRuntimeError};
use std::ffi::c_void;
use std::io::Read;
use std::os::windows::io::FromRawHandle;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, LocalFree, HANDLE, HLOCAL, WAIT_OBJECT_0,
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
    FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_TRAVERSE,
};
use windows::Win32::Security::{
    FreeSid, SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES, PSID,
};
use windows::Win32::System::Console::{ClosePseudoConsole, CreatePseudoConsole, COORD, HPCON};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE,
    REG_DWORD, REG_OPTION_NON_VOLATILE,
};
use windows::Win32::System::Pipes::CreatePipe;
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_UNICODE_ENVIRONMENT,
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

/// 幂等设 HKCU\Console\LowBoxConsoleEnabled=1:放行 AppContainer(LowBox)进程的控制台连接,
/// 是方案 B 让容器内 native console 程序输出能回传的前提(配合 lpacCom 能力 + ConPTY)。
unsafe fn ensure_lowbox_console_enabled() {
    let mut hkey = HKEY::default();
    let subkey = wide("Console");
    let rc = RegCreateKeyExW(
        HKEY_CURRENT_USER,
        PCWSTR(subkey.as_ptr()),
        0,
        PCWSTR::null(),
        REG_OPTION_NON_VOLATILE,
        KEY_SET_VALUE,
        None,
        &mut hkey,
        None,
    );
    if rc.is_ok() {
        let name = wide("LowBoxConsoleEnabled");
        let val: u32 = 1;
        let _ = RegSetValueExW(
            hkey,
            PCWSTR(name.as_ptr()),
            0,
            REG_DWORD,
            Some(&val.to_le_bytes()),
        );
        let _ = RegCloseKey(hkey);
    }
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

/// 给目录授「仅遍历(FILE_TRAVERSE)、不继承」的 ACE：让容器能穿过此目录到达更深的已授权目录,
/// 但不能列此目录内容、也不传播给子项。用于给工作区父链开路——否则 PowerShell `Set-Location`
/// 到用户 profile 路径下的工作区会「拒绝访问」(AppContainer 默认对用户目录链遍历受限)。
unsafe fn grant_traverse_to_appcontainer(path: &Path, ac_sid: PSID) -> Result<(), ToolRuntimeError> {
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
        return Err(err(format!("GetNamedSecurityInfo(traverse) 失败: {}", rc.0)));
    }
    let ea = EXPLICIT_ACCESS_W {
        grfAccessPermissions: FILE_TRAVERSE.0,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: windows::Win32::Security::ACE_FLAGS(0), // 不继承
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_UNKNOWN,
            ptstrName: PWSTR(ac_sid.0 as *mut u16),
        },
    };
    let mut new_dacl: *mut ACL = std::ptr::null_mut();
    let rc = SetEntriesInAclW(Some(&[ea]), Some(old_dacl as *const ACL), &mut new_dacl);
    if rc.0 != 0 {
        return Err(err(format!("SetEntriesInAcl(traverse) 失败: {}", rc.0)));
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
    let _ = LocalFree(HLOCAL(new_dacl as *mut c_void));
    if !sd.0.is_null() {
        let _ = LocalFree(HLOCAL(sd.0));
    }
    if rc.0 != 0 {
        return Err(err(format!("SetNamedSecurityInfo(traverse) 失败: {}", rc.0)));
    }
    Ok(())
}

/// 把 ConPTY 输出的 VT/ANSI 转义流剥离为纯文本:去掉 CSI(ESC[..final)、OSC(ESC]..BEL/ST)、
/// 其它 ESC 序列;`\r\n` 归一为 `\n`,丢弃裸 `\r`(伪控制台的光标回车)。
pub(crate) fn strip_vt(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\u{1b}' {
            match chars.get(i + 1) {
                Some('[') => {
                    // CSI: ESC [ ...参数/中间字节... final(0x40..=0x7e)
                    i += 2;
                    while i < chars.len() {
                        let f = chars[i];
                        i += 1;
                        if ('\u{40}'..='\u{7e}').contains(&f) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    // OSC: ESC ] ... (BEL | ESC \)
                    i += 2;
                    while i < chars.len() {
                        if chars[i] == '\u{07}' {
                            i += 1;
                            break;
                        }
                        if chars[i] == '\u{1b}' && chars.get(i + 1) == Some(&'\\') {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                Some(_) => i += 2, // 其它 ESC x:跳过两字符
                None => i += 1,
            }
            continue;
        }
        if c == '\r' {
            i += 1; // 丢弃裸 \r(\r\n 的 \n 下一轮保留)
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// 在 AppContainer 中执行命令并捕获输出。隔离层次:真 AppContainer 令牌(Low IL + 默认拒绝文件/网络)
/// + 工作区 ACL 授权(容器仅能读写工作区) + 能力 SID 网络门控(allow_network 决定能否出站)。
///
/// 输出走 ConPTY 伪控制台:配合 LowBoxConsoleEnabled + lpacCom/registryRead 能力,容器内 native console
/// 程序(git/npm/cmd 等)的输出才能回传(纯管道在 AppContainer 内对 native 孙进程会丢)。命令前用
/// New-PSDrive 把工作区挂成 PSDrive 并 Set-Location 进去——规避 AppContainer 下 Set-Location 到用户
/// profile 路径的「拒绝访问」,使 native cwd 与 cmdlet 相对路径都落在工作区。输出为 VT 流,strip_vt 还原。
///
/// fail-closed:任一步骤失败返回 Err,由 run_command_streaming 决定是否降级到受限令牌沙箱。
pub fn run_in_appcontainer_sandbox(
    workspace: &Path,
    command: &str,
    timeout: Duration,
    on_line: Option<CommandLineCallback>,
    cancel: Option<CommandCancel>,
    allow_network: bool,
) -> Result<RunCommandResult, ToolRuntimeError> {
    unsafe {
        // 放行 LowBox 控制台(方案 B 前提,幂等设 HKCU\Console\LowBoxConsoleEnabled=1)。
        ensure_lowbox_console_enabled();
        // 能力:registryRead(读 HKCU\Console\LowBoxConsoleEnabled)+ lpacCom(放行 native 连 conhost)
        // (+ allow_network 时加 internetClient/Server/privateNetwork)。能力须在 profile 创建期与进程
        // 启动期都登记,仅启动期传入不会真正落入令牌。
        let mut cap_sids: Vec<PSID> = Vec::new();
        let mut caps: Vec<SID_AND_ATTRIBUTES> = Vec::new();
        let mut cap_strs: Vec<&str> = vec![
            "S-1-15-3-1024-1065365936-1281604716-3511738428-1654721687-432734479-3232135806-4053264122-3456934681", // registryRead
            "S-1-15-3-1024-2405443489-874036122-4286035555-1823921565-1746547431-2453885448-3625952902-991631256",  // lpacCom
        ];
        if allow_network {
            cap_strs.extend(["S-1-15-3-1", "S-1-15-3-2", "S-1-15-3-3"]);
        }
        for s in &cap_strs {
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
        let caps_arg: Option<&[SID_AND_ATTRIBUTES]> =
            if caps.is_empty() { None } else { Some(&caps[..]) };
        // 容器名按能力集区分,避免「已存在但能力集不符」的派生陷阱。
        let (name, display) = if allow_network {
            ("MDGA.Sandbox.Lpac.Net", "MDGA sandbox (lpac, net)")
        } else {
            ("MDGA.Sandbox.Lpac", "MDGA sandbox (lpac)")
        };
        let ac_sid = create_or_derive_appcontainer_sid(name, display, caps_arg)?;
        // 授权工作区目录(容器仅能读写工作区)+ 工作区父链 traverse(New-PSDrive 解析工作区路径需要)。
        grant_dir_access_to_appcontainer(workspace, ac_sid)?;
        let mut anc = workspace.parent();
        while let Some(dir) = anc {
            let _ = grant_traverse_to_appcontainer(dir, ac_sid);
            anc = dir.parent();
        }
        // 容器专用临时目录:授权 + 接管 TEMP/TMP(容器读不到用户 %TEMP%,而 PowerShell/.NET/工具需可写 TEMP)。
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

        // ConPTY 管道:in(我们写→PTY)、out(PTY→我们读)。
        let (mut in_r, mut in_w) = (HANDLE::default(), HANDLE::default());
        let (mut out_r, mut out_w) = (HANDLE::default(), HANDLE::default());
        CreatePipe(&mut in_r, &mut in_w, None, 0)
            .map_err(|e| err(format!("CreatePipe(in) 失败: {e}")))?;
        CreatePipe(&mut out_r, &mut out_w, None, 0)
            .map_err(|e| err(format!("CreatePipe(out) 失败: {e}")))?;
        let coord = COORD { X: 160, Y: 50 };
        let hpc: HPCON = CreatePseudoConsole(coord, in_r, out_w, 0)
            .map_err(|e| err(format!("CreatePseudoConsole 失败: {e}")))?;
        let _ = CloseHandle(in_r);
        let _ = CloseHandle(out_w);

        // 属性列表:SECURITY_CAPABILITIES + PSEUDOCONSOLE。
        let mut attr_size: usize = 0;
        let _ = InitializeProcThreadAttributeList(
            LPPROC_THREAD_ATTRIBUTE_LIST(std::ptr::null_mut()),
            2,
            0,
            &mut attr_size,
        );
        let mut attr_buf = vec![0u8; attr_size];
        let attr_list = LPPROC_THREAD_ATTRIBUTE_LIST(attr_buf.as_mut_ptr() as *mut c_void);
        InitializeProcThreadAttributeList(attr_list, 2, 0, &mut attr_size)
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
        .map_err(|e| err(format!("UpdateProcThreadAttribute(caps) 失败: {e}")))?;
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

        // STARTUPINFOEXW:NULL std 句柄,强制子进程用伪控制台(不经 PEB 继承父进程的重定向句柄)。
        let mut si = STARTUPINFOEXW::default();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = HANDLE::default();
        si.StartupInfo.hStdOutput = HANDLE::default();
        si.StartupInfo.hStdError = HANDLE::default();
        si.lpAttributeList = attr_list;

        // 命令包装:New-PSDrive 把工作区挂成 PSDrive 并 Set-Location 进去,再跑用户命令。
        let ws_root = workspace.as_os_str().to_string_lossy().replace('\'', "''");
        let wrapped = format!(
            "New-PSDrive -Name MWS -PSProvider FileSystem -Root '{ws}' -Scope Global -ErrorAction SilentlyContinue | Out-Null; Set-Location MWS: -ErrorAction SilentlyContinue; {cmd}",
            ws = ws_root,
            cmd = command
        );
        let mut cmdline = crate::sandbox_win::encoded_command_line(&wrapped);
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
            return Err(err(format!("CreateProcess(AppContainer/ConPTY) 失败: {e}")));
        }

        // 并发读伪控制台输出(VT 流);进程退出后关 PTY 触发 EOF,reader 收尾。
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
        ClosePseudoConsole(hpc);
        let _ = CloseHandle(in_w);
        let raw = reader.join().unwrap_or_default();
        let _ = CloseHandle(pi.hThread);
        let _ = CloseHandle(pi.hProcess);
        FreeSid(ac_sid);
        let duration_ms = started.elapsed().as_millis();

        // VT 流剥离为纯文本,截断到 64KiB(char 边界);按行触发 on_line 回调。
        let mut text = strip_vt(&String::from_utf8_lossy(&raw));
        let mut truncated = false;
        const MAX: usize = 64 * 1024;
        if text.len() > MAX {
            let mut end = MAX;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            truncated = true;
        }
        if let Some(cb) = on_line.as_ref() {
            for line in text.lines() {
                cb(line.to_string());
            }
        }

        Ok(RunCommandResult {
            command: command.to_string(),
            exit_code: if timed_out { None } else { Some(exit_code as i32) },
            stdout: text,
            stderr: String::new(), // ConPTY 合并 stdout/stderr 到单一终端流
            truncated,
            timed_out,
            duration_ms,
        })
    }
}

/// 运行时能力自检:在临时区跑探针,实测**本机** AppContainer 方案能否回传 native console 输出。
///
/// 各版本 Windows 对 LowBoxConsoleEnabled / ConPTY / AppContainer / lpacCom 的支持不一(我们只在
/// 一台 Win11 上验证过),与其按版本号猜,不如实跑一条 `cmd echo <token>` 看输出是否回传——回传则
/// AppContainer 全功能可用,否则由调用方 fail-closed 降级到受限令牌沙箱。结果应由调用方缓存(每进程
/// 探一次即可)。探针自带 ensure_lowbox_console_enabled(经 run_in_appcontainer_sandbox),故也顺带
/// 验证了注册表开关在本机是否真生效。
pub fn appcontainer_self_test() -> bool {
    let probe_ws = std::env::temp_dir().join("mdga-ac-selftest");
    if std::fs::create_dir_all(&probe_ws).is_err() {
        return false;
    }
    // nonce(纳秒时戳)防误判:避免历史/缓存输出里残留的 token 被当成探针成功。
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let token = format!("MDGA_SELFTEST_{nonce:x}");
    let r = run_in_appcontainer_sandbox(
        &probe_ws,
        &format!("cmd /c echo {token}"),
        Duration::from_secs(15),
        None,
        None,
        false,
    );
    let _ = std::fs::remove_dir_all(&probe_ws);
    matches!(r, Ok(o) if o.stdout.contains(token.as_str()))
}

/// PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE 的原始值(windows 0.58 未必导出常量,直接用数值)。
const PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE_VAL: usize = 0x0002_0016;

#[cfg(test)]
mod tests {
    use super::*;

    /// AppContainer 测试共享机器级全局状态(容器 profile、共享沙箱 temp 目录的 DACL),
    /// 并行会相互踩踏(授权读改写竞态、profile 并发创建)。用此锁串行化各容器测试。
    static SANDBOX_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        SANDBOX_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[test]
    fn strip_vt_removes_escapes() {
        assert_eq!(strip_vt("\u{1b}[?25l\u{1b}[2Jhello\u{1b}[m"), "hello");
        assert_eq!(strip_vt("\u{1b}]0;title\u{07}world"), "world");
        assert_eq!(strip_vt("a\r\nb\r\n"), "a\nb\n");
        assert_eq!(strip_vt("plain text 42"), "plain text 42");
        assert_eq!(strip_vt("\u{1b}]0;t\u{1b}\\X"), "X");
    }

    /// 运行时自检在本机应通过(方案B可用)——这是 dispatch 决定走 AppContainer 还是降级受限令牌的依据。
    #[test]
    fn appcontainer_self_test_passes_here() {
        let _g = test_guard();
        assert!(
            appcontainer_self_test(),
            "本机 AppContainer 自检未通过——dispatch 会降级受限令牌(若预期可用,查 LowBoxConsole/能力/ConPTY)"
        );
    }

    /// 生产路径端到端:run_in_appcontainer_sandbox(ConPTY + lpacCom + LowBoxConsole + New-PSDrive)
    /// 跑 native 命令,验证(1)在工作区 cwd 跑(相对路径读到工作区文件) (2)native 输出经伪控制台回传、
    /// 已 strip_vt 成纯文本。这是方案 B 完整接通的回归测试。
    #[test]
    fn appcontainer_sandbox_runs_native_in_workspace() {
        let _g = test_guard();
        let ws = std::env::temp_dir().join(format!("mdga-ac-prod-{}", std::process::id()));
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join("hello.txt"), "PRODFILE_88").unwrap();
        let r = run_in_appcontainer_sandbox(
            &ws,
            "cmd /c type hello.txt",
            Duration::from_secs(30),
            None,
            None,
            false,
        );
        let _ = std::fs::remove_dir_all(&ws);
        let out = r.expect("起进程失败(环境可能不支持 AppContainer)");
        eprintln!("[prod] exit={:?} stdout=[{}]", out.exit_code, out.stdout.trim());
        assert!(
            out.stdout.contains("PRODFILE_88"),
            "生产沙箱:native 命令未在工作区读到文件/输出未回传:\n{}",
            out.stdout
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

    /// native 命令能在容器内跑、能写工作区文件(cmd 自身 `>` 重定向)。与 appcontainer_sandbox_runs_
    /// native_in_workspace(测 stdout 经伪控制台回传)互补:本测试聚焦"命令确实执行并写盘"。
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
