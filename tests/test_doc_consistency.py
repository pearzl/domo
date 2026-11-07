#!/usr/bin/env python3
"""文档与实现约定的一致性守卫（只读检查，不启动服务）。

只检查可以稳定机械判断的约定，避免把易变的措辞写进测试：
  - README / DESIGN / TECH 不出现已移除的术语（直接完成、user_action、rusqlite、WorkBuddy 等）；
  - 关键技术事实仍被文档记录（SQLite、CodeBuddy、5,000 字共享记忆、合入冲突 3 次、deliver/execute）；
  - handoff 系列已合并为单一设计记录，不再保留过程文档。
不检查具体段落位置或完整措辞；文档更新后按事实调整本文件。
退出码 0 表示全部通过。
"""
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOCS = [ROOT / "README.md", ROOT / "docs" / "DESIGN.md", ROOT / "docs" / "TECH.md"]

BANNED = [
    "rusqlite",
    "WorkBuddy",
    "user_action",
    "顺序即优先级",
    "输入/中间/交付",
    "人工添加任务",
    "直接完成",
]

REQUIRED = {
    ROOT / "README.md": ["SQLite", "CodeBuddy"],
    ROOT / "docs" / "DESIGN.md": ["deliver", "execute", "5k"],
    ROOT / "docs" / "TECH.md": ["5,000", "3 次", "issues"],
}


def main():
    for path in DOCS:
        text = path.read_text(encoding="utf-8")
        for term in BANNED:
            assert term not in text, f"{path.relative_to(ROOT)} 仍包含已移除术语: {term}"
        for term in REQUIRED.get(path, []):
            assert term in text, f"{path.relative_to(ROOT)} 缺少约定术语: {term}"
    notes = ROOT / "docs" / "notes"
    leftovers = sorted(p.name for p in notes.glob("handoff-h*.md"))
    assert not leftovers, f"handoff 过程文档未归档: {leftovers}"
    assert (notes / "handoff-design-record.md").is_file(), "缺少交接设计记录"
    print("doc consistency ok")


if __name__ == "__main__":
    main()
