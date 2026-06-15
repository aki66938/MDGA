// 侧边栏视图组件（行为敏感解耦增量 1，Task B）：从 App() 纯搬移侧栏 JSX。
// 铁律：无业务逻辑，仅渲染 + 调 props；JSX/className/文案/结构与原 App 内完全一致。
import {
  SquarePen, Search, Pin, Archive, ArchiveRestore, Trash2, Settings2,
  Sun, Moon, ChevronDown, ChevronRight,
} from "lucide-react";
import { BrandMark } from "./dialogs";
import type { Conversation, UpdateState } from "../types";

export type SidebarProps = {
  // 会话列表
  conversations: Conversation[];
  visibleConversations: Conversation[];
  archivedConversations: Conversation[];
  activeConvId: string | null;
  // 搜索
  searchQuery: string;
  onSearchChange: (value: string) => void;
  // 归档区展开
  showArchived: boolean;
  onToggleArchived: () => void;
  // 行内重命名
  editingConvId: string | null;
  editingTitle: string;
  onEditingTitleChange: (value: string) => void;
  onCommitRename: () => void;
  onCancelRename: () => void;
  onStartRename: (e: React.MouseEvent, conv: Conversation) => void;
  // 会话 handler
  onNewConversation: () => void;
  onSelectConversation: (id: string) => void;
  onDeleteConversation: (e: React.MouseEvent, id: string) => void;
  onTogglePin: (e: React.MouseEvent, conv: Conversation) => void;
  onToggleArchive: (e: React.MouseEvent, conv: Conversation) => void;
  // 主题
  theme: "light" | "dark";
  onToggleTheme: () => void;
  // 设置
  onOpenSettings: () => void;
  // 更新横幅
  update: UpdateState;
  onInstallUpdate: () => void;
  onDismissUpdate: () => void;
};

export function Sidebar(props: SidebarProps) {
  const {
    conversations, visibleConversations, archivedConversations, activeConvId,
    searchQuery, onSearchChange, showArchived, onToggleArchived,
    editingConvId, editingTitle, onEditingTitleChange, onCommitRename, onCancelRename, onStartRename,
    onNewConversation, onSelectConversation, onDeleteConversation, onTogglePin, onToggleArchive,
    theme, onToggleTheme, onOpenSettings, update, onInstallUpdate, onDismissUpdate,
  } = props;

  function renderConvItem(conv: Conversation) {
    return (
      <div
        key={conv.id}
        className={`conv-item${conv.id === activeConvId ? " conv-item--active" : ""}`}
        onClick={() => onSelectConversation(conv.id)}
        role="button"
        tabIndex={0}
        onKeyDown={(e) => e.key === "Enter" && onSelectConversation(conv.id)}
      >
        {editingConvId === conv.id ? (
          <input
            className="conv-item__rename"
            value={editingTitle}
            autoFocus
            onChange={(e) => onEditingTitleChange(e.target.value)}
            onClick={(e) => e.stopPropagation()}
            onBlur={onCommitRename}
            onKeyDown={(e) => {
              if (e.key === "Enter") onCommitRename();
              if (e.key === "Escape") onCancelRename();
            }}
          />
        ) : (
          <span
            className="conv-item__title"
            onDoubleClick={(e) => onStartRename(e, conv)}
            title="双击重命名"
          >
            {conv.pinned && <Pin size={11} className="conv-item__pin-mark" />}
            {conv.title}
          </span>
        )}
        <span className="conv-item__actions">
          <button
            className="conv-item__action"
            type="button"
            aria-label={conv.pinned ? "取消置顶" : "置顶"}
            title={conv.pinned ? "取消置顶" : "置顶"}
            onClick={(e) => onTogglePin(e, conv)}
          >
            <Pin size={14} />
          </button>
          <button
            className="conv-item__action"
            type="button"
            aria-label={conv.archived ? "取消归档" : "归档"}
            title={conv.archived ? "取消归档" : "归档"}
            onClick={(e) => onToggleArchive(e, conv)}
          >
            {conv.archived ? <ArchiveRestore size={14} /> : <Archive size={14} />}
          </button>
          <button
            className="conv-item__delete"
            type="button"
            aria-label="删除会话"
            title="删除"
            onClick={(e) => onDeleteConversation(e, conv.id)}
          >
            <Trash2 size={14} />
          </button>
        </span>
      </div>
    );
  }

  return (
    <aside className="sidebar" aria-label="MDGA navigation">
      <div className="brand-row">
        <BrandMark size={22} />
        <span className="brand-row__name">MDGA</span>
      </div>
      <button className="new-chat" type="button" onClick={onNewConversation}>
        <SquarePen size={16} /> 新对话
      </button>

      {conversations.length > 0 && (
        <nav className="conv-list" aria-label="会话列表">
          <div className="conv-search-wrap">
            <Search size={14} className="conv-search-wrap__icon" />
            <input
              className="conv-search"
              type="search"
              placeholder="搜索会话…"
              value={searchQuery}
              onChange={(e) => onSearchChange(e.target.value)}
              aria-label="搜索会话"
            />
          </div>
          <p className="nav-label">历史对话</p>
          {visibleConversations.map(renderConvItem)}
          {archivedConversations.length > 0 && (
            <>
              <button
                className="archived-toggle"
                type="button"
                onClick={onToggleArchived}
              >
                {showArchived ? <ChevronDown size={13} /> : <ChevronRight size={13} />} 已归档（{archivedConversations.length}）
              </button>
              {showArchived && archivedConversations.map(renderConvItem)}
            </>
          )}
        </nav>
      )}

      {update.status === "available" && (
        <div className="update-banner">
          <p className="update-banner__title">发现新版本</p>
          <p className="update-banner__version">v{update.version}</p>
          <div className="update-banner__actions">
            <button className="update-banner__btn update-banner__btn--primary" type="button" onClick={onInstallUpdate}>
              立即更新
            </button>
            <button className="update-banner__btn" type="button" onClick={onDismissUpdate}>
              稍后
            </button>
          </div>
        </div>
      )}

      {update.status === "downloading" && (
        <div className="update-banner">
          <p className="update-banner__title">正在下载更新…</p>
          <div className="update-banner__progress-bar">
            <div className="update-banner__progress-fill" style={{ width: `${update.progress}%` }} />
          </div>
          <p className="update-banner__version">{update.progress}%</p>
        </div>
      )}

      {update.status === "error" && (
        <div className="update-banner update-banner--error">
          <p className="update-banner__title">更新失败</p>
          <p className="update-banner__version">{update.message}</p>
          <button className="update-banner__btn" type="button" onClick={onDismissUpdate}>
            关闭
          </button>
        </div>
      )}

      {/* 侧边栏底部 footer：设置 + 主题切换 */}
      <div className="sidebar-footer">
        <button className="sidebar-footer__btn" type="button" onClick={onOpenSettings}>
          <Settings2 size={16} /> 设置
        </button>
        <button
          className="sidebar-footer__icon"
          type="button"
          aria-label={theme === "dark" ? "切换到亮色" : "切换到深色"}
          title={theme === "dark" ? "切换到亮色" : "切换到深色「深海」"}
          onClick={onToggleTheme}
        >
          {theme === "dark" ? <Sun size={16} /> : <Moon size={16} />}
        </button>
      </div>
    </aside>
  );
}
