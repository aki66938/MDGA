# Plan09 - MDGA Sandbox Runtime

项目代号：MDGA  
文档定位：本文件定义 MDGA 本地进程级沙箱技术路线。默认路线不采用 Docker 或微型虚拟机，而是优先使用 OS-level / process-level sandbox。

---

## 1. 设计目标

Sandbox Runtime 的目标是在贴近用户真实本地环境的同时，限制 Agent 启动的命令、脚本和工具进程。

目标：

- 限制文件读写范围。
- 限制网络访问。
- 限制子进程继承能力。
- 保留真实开发环境可用性。
- 资源占用低于 Docker / VM。
- 与 Permission Manager 和 Audit Layer 组合使用。

---

## 2. 沙箱分层

MDGA 沙箱不是单独一层完成所有安全：

- Permission Layer：决定是否允许发起动作。
- Sandbox Policy Layer：把产品权限转成 OS 执行策略。
- Process Isolation Layer：启动受限子进程。
- Audit Layer：记录实际执行。

原则：

- Permission 允许不代表进程无限制。
- Full Access 可以放宽策略，但仍保留审计。
- 沙箱失败不能自动降级为无限制执行。

---

## 3. Windows 路线

Windows MVP 优先调研：

- Restricted Token。
- 专用低权限本地用户。
- ACL。
- Windows Firewall。
- 受控 command runner。

技术依据：

- Windows restricted token 可以通过禁用 SID、删除 privileges、添加 restricting SIDs 限制进程能力。
- ACL 可控制文件和目录访问。
- Windows Firewall 可参与网络边界控制。

参考入口：

- [Microsoft Restricted Tokens](https://learn.microsoft.com/en-us/windows/win32/secauthz/restricted-tokens)
- [CreateRestrictedToken](https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-createrestrictedtoken)
- [Microsoft Access Control Lists](https://learn.microsoft.com/en-us/windows/win32/secauthz/access-control-lists)
- [Windows Firewall Rules](https://learn.microsoft.com/en-us/windows/security/operating-system-security/network-security/windows-firewall/configure)

---

## 4. macOS 路线

macOS 后续调研：

- Seatbelt / sandbox profile。
- App Sandbox entitlement。
- 安全书签与用户授权路径。

注意：

- `sandbox-exec` 在现代 macOS 中存在弃用争议，不能作为长期唯一依据。
- 正式产品需要评估签名、entitlement、App Store 和非 App Store 分发差异。

---

## 5. Linux / WSL2 路线

Linux / WSL2 后续调研：

- bubblewrap。
- mount namespace。
- user namespace。
- 只读 bind。
- 临时可写目录。
- 网络 namespace 或禁网策略。

参考入口：

- [bubblewrap GitHub](https://github.com/containers/bubblewrap)

bubblewrap 是构造沙箱环境的低层工具，不是完整安全策略。MDGA 需要自己定义 policy。

---

## 6. Sandbox Policy Schema

统一策略建议：

- `policy_id`
- `workspace_root`
- `read_paths`
- `write_paths`
- `deny_paths`
- `network_mode`
- `allowed_domains`
- `allowed_commands`
- `env_allowlist`
- `timeout_ms`
- `inherit_child_process_policy`
- `permission_mode`

原则：

- 默认不继承敏感环境变量。
- 默认禁网。
- 默认只写工作区或任务临时目录。
- 子进程继承同一限制。

---

## 7. MVP 最小能力

Windows MVP 最小目标：

- 在授权工作区内创建文件。
- 拒绝写入工作区外路径。
- 记录执行命令和结果。
- 默认不把 `DEEPSEEK_API_KEY` 传给子进程。
- 沙箱能力未实现时，清楚标注当前运行级别。

不承诺：

- 第一版完整防恶意攻击。
- 第一版完全网络隔离。
- 第一版跨平台沙箱统一完成。

---

## 8. Docker / VM 定位

Docker / VM 不作为默认体验。

适用场景：

- 企业版强隔离。
- 高风险未知项目。
- 完整可复现环境。
- 安全研究模式。

原因：

- 启动慢。
- 资源占用高。
- 本地环境桥接复杂。
- 普通用户理解成本高。

---

## 9. 验收标准

MVP 验收：

- 沙箱策略对象可生成。
- 文件写入能被工作区边界约束。
- 子进程环境变量经过过滤。
- 执行日志可审计。
- 沙箱失败不会静默放行。
- UI 能显示当前隔离能力等级。

---

## 10. 当前结论

MDGA 应走本地进程级沙箱路线。它不是绝对安全承诺，而是在性能、真实本地体验和风险控制之间的合理工程平衡。第一版先把策略、边界、审计和失败不降级立住，再逐步增强各平台隔离强度。
