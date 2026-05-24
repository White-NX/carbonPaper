"""
Classification service — text embedding with BAAI/bge-small-zh-v1.5 + anchor KNN.

Uses a set of pre-defined anchor texts per category.  Each new screenshot's
title (and optionally OCR text) is embedded and compared against the anchors
via **weighted** cosine similarity to determine the most likely category.

Anchor format (v2 – structured):
    { "category_name": [ { "text": "...", "source": "default|user_feedback|ocr_feedback",
                            "weight": 1.0, "added_at": "..." }, ... ] }

Supports:
    - Weighted scoring  (user_feedback=2.0 > ocr_feedback=1.5 > default=1.0)
    - Negative feedback  (remove anchor from old category on re-classification)
    - Semantic deduplication  (skip if cosine > 0.95 with existing anchor)
    - OCR auxiliary anchors  (add OCR text when sufficiently different from title)
"""

import os
import re
import json
import logging
import time
import numpy as np
from typing import Tuple, List, Dict, Optional, Any

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Weight scheme constants
# ---------------------------------------------------------------------------
WEIGHT_DEFAULT = 1.0
WEIGHT_USER_FEEDBACK = 2.0
WEIGHT_OCR_FEEDBACK = 1.5
DEDUP_COSINE_THRESHOLD = 0.95   # Skip anchor if cosine > this with existing
OCR_DIVERSITY_THRESHOLD = 0.7   # Add OCR as auxiliary anchor only if cosine < this with title
OCR_MIN_LENGTH = 20             # Minimum OCR text length to consider as auxiliary anchor

# ---------------------------------------------------------------------------
# Process → category prior — for "single-purpose" apps where the process name
# alone is a strong signal of the activity. Applied as a small additive bonus
# on the final blended score. Do not add multi-purpose processes (browsers,
# Steam, Office suite, etc.) here — their content drives the category.
# ---------------------------------------------------------------------------
PROCESS_CATEGORY_PRIOR: Dict[str, str] = {
    # ---- 社交通讯 ----
    "qq.exe": "社交通讯",
    "tim.exe": "社交通讯",
    "wechat.exe": "社交通讯",
    "weixin.exe": "社交通讯",
    "wxwork.exe": "社交通讯",
    "dingtalk.exe": "社交通讯",
    "feishu.exe": "社交通讯",
    "lark.exe": "社交通讯",
    "telegram.exe": "社交通讯",
    "whatsapp.exe": "社交通讯",
    "slack.exe": "社交通讯",
    "outlook.exe": "社交通讯",
    "thunderbird.exe": "社交通讯",
    "foxmail.exe": "社交通讯",
    "ms-teams.exe": "社交通讯",

    # ---- 编程开发 ----
    "code.exe": "编程开发",
    "code - insiders.exe": "编程开发",
    "cursor.exe": "编程开发",
    "windsurf.exe": "编程开发",
    "idea64.exe": "编程开发",
    "idea.exe": "编程开发",
    "pycharm64.exe": "编程开发",
    "pycharm.exe": "编程开发",
    "webstorm64.exe": "编程开发",
    "goland64.exe": "编程开发",
    "clion64.exe": "编程开发",
    "phpstorm64.exe": "编程开发",
    "rubymine64.exe": "编程开发",
    "rider64.exe": "编程开发",
    "studio64.exe": "编程开发",
    "devenv.exe": "编程开发",
    "sublime_text.exe": "编程开发",
    "gvim.exe": "编程开发",
    "nvim.exe": "编程开发",
    "postman.exe": "编程开发",
    "insomnia.exe": "编程开发",
    "dbeaver.exe": "编程开发",
    "navicat.exe": "编程开发",
    "ssms.exe": "编程开发",
    "github desktop.exe": "编程开发",
    "sourcetree.exe": "编程开发",
    "gitkraken.exe": "编程开发",
    "fork.exe": "编程开发",
    "docker desktop.exe": "编程开发",
    "windowsterminal.exe": "编程开发",
    "wezterm-gui.exe": "编程开发",
    "alacritty.exe": "编程开发",

    # ---- 办公文档 ----
    "winword.exe": "办公文档",
    "excel.exe": "办公文档",
    "powerpnt.exe": "办公文档",
    "onenote.exe": "办公文档",
    "visio.exe": "办公文档",
    "mspub.exe": "办公文档",
    "wps.exe": "办公文档",
    "wpsoffice.exe": "办公文档",
    "et.exe": "办公文档",
    "wpp.exe": "办公文档",
    "foxitreader.exe": "办公文档",
    "foxitphantompdf.exe": "办公文档",
    "acrord32.exe": "办公文档",
    "acrobat.exe": "办公文档",
    "sumatrapdf.exe": "办公文档",
    "pdfxedit.exe": "办公文档",
    "evernote.exe": "办公文档",
    "typora.exe": "办公文档",
    "obsidian.exe": "办公文档",
    "notion.exe": "办公文档",
    "logseq.exe": "办公文档",

    # ---- 设计创作 ----
    "photoshop.exe": "设计创作",
    "illustrator.exe": "设计创作",
    "indesign.exe": "设计创作",
    "adobe premiere pro.exe": "设计创作",
    "premierepro.exe": "设计创作",
    "afterfx.exe": "设计创作",
    "audition.exe": "设计创作",
    "adobe media encoder.exe": "设计创作",
    "lightroom.exe": "设计创作",
    "xd.exe": "设计创作",
    "blender.exe": "设计创作",
    "3dsmax.exe": "设计创作",
    "maya.exe": "设计创作",
    "zbrush.exe": "设计创作",
    "cinema 4d.exe": "设计创作",
    "c4d.exe": "设计创作",
    "figma.exe": "设计创作",
    "sketch.exe": "设计创作",
    "jianyingpro.exe": "设计创作",
    "resolve.exe": "设计创作",
    "krita.exe": "设计创作",
    "inkscape.exe": "设计创作",
    "gimp-2.10.exe": "设计创作",
    "acad.exe": "设计创作",
    "sketchup.exe": "设计创作",

    # ---- 游戏 ----
    "steam.exe": "游戏",
    "epicgameslauncher.exe": "游戏",
    "battle.net.exe": "游戏",
    "wegame.exe": "游戏",
    "ubisoftconnect.exe": "游戏",
    "gog galaxy.exe": "游戏",
    "league of legends.exe": "游戏",
    "leagueclient.exe": "游戏",
    "valorant.exe": "游戏",
    "valorant-win64-shipping.exe": "游戏",
    "dota2.exe": "游戏",
    "cs2.exe": "游戏",
    "csgo.exe": "游戏",
    "minecraft.exe": "游戏",
    "minecraftlauncher.exe": "游戏",
    "minecraft launcher.exe": "游戏",
    "yuanshen.exe": "游戏",
    "genshinimpact.exe": "游戏",
    "starrail.exe": "游戏",
    "bh3.exe": "游戏",
    "zenlesszonezero.exe": "游戏",
    "wzryclient.exe": "游戏",
    "r5apex.exe": "游戏",
    "tslgame.exe": "游戏",
    "gta5.exe": "游戏",
    "rdr2.exe": "游戏",
    "eldenring.exe": "游戏",
    "bluestacks.exe": "游戏",
    "ldplayer.exe": "游戏",
    "ldplayer9.exe": "游戏",
    "mumumultiplayer.exe": "游戏",
    "xxmi launcher.exe": "游戏",

    # ---- 影音娱乐 ----
    "cloudmusic.exe": "影音娱乐",
    "qqmusic.exe": "影音娱乐",
    "kugou.exe": "影音娱乐",
    "spotify.exe": "影音娱乐",
    "foobar2000.exe": "影音娱乐",
    "aimp.exe": "影音娱乐",
    "potplayer.exe": "影音娱乐",
    "potplayermini64.exe": "影音娱乐",
    "vlc.exe": "影音娱乐",
    "mpc-hc.exe": "影音娱乐",
    "mpc-be.exe": "影音娱乐",
    "bilibili.exe": "影音娱乐",
    "iqiyi.exe": "影音娱乐",
    "youku.exe": "影音娱乐",
    "qqlive.exe": "影音娱乐",
    "douyin.exe": "影音娱乐",
    "twitch.exe": "影音娱乐",

    # ---- 学习教育 ----
    "anki.exe": "学习教育",
    "youdaodict.exe": "学习教育",

    # ---- 阅读资讯 ----
    "weread.exe": "阅读资讯",
    "kindle.exe": "阅读资讯",
    "zotero.exe": "阅读资讯",
    "mendeley.exe": "阅读资讯",
    "endnote.exe": "阅读资讯",
}
PROCESS_PRIOR_BONUS = 0.12

# ---------------------------------------------------------------------------
# Anchor data type helper
# ---------------------------------------------------------------------------

def _make_anchor(
    text: str,
    source: str = "default",
    weight: float = WEIGHT_DEFAULT,
    scope: str = "global",
    process_name: Optional[str] = None,
) -> Dict[str, Any]:
    """Create a structured anchor entry."""
    return {
        "text": text,
        "source": source,
        "weight": weight,
        "scope": scope,
        "process_name": process_name,
        "added_at": time.strftime("%Y-%m-%dT%H:%M:%S"),
    }


# ---------------------------------------------------------------------------
# Default anchors (cold-start) — structured format
# ---------------------------------------------------------------------------

DEFAULT_ANCHORS: Dict[str, List[Dict[str, Any]]] = {
    "编程开发": [
        # 真实窗口标题 / 应用名
        _make_anchor("main.py - Visual Studio Code"),
        _make_anchor("app.tsx - Cursor"),
        _make_anchor("MainActivity.kt - Android Studio"),
        _make_anchor("项目 [run] - IntelliJ IDEA"),
        _make_anchor("Pull Request - GitHub"),
        _make_anchor("Visual Studio Code 编辑器"),
        _make_anchor("终端 - bash"),
        _make_anchor("Windows PowerShell 终端"),
        _make_anchor("Docker Desktop 容器管理"),
        _make_anchor("Postman API 接口调试"),
        _make_anchor("DBeaver 数据库连接"),
        # 活动短语
        _make_anchor("Python 异步编程原理"),
        _make_anchor("Git 分支合并冲突解决"),
        _make_anchor("React 组件状态管理"),
        _make_anchor("Rust 内存所有权与借用"),
        _make_anchor("SQL 查询性能优化"),
        _make_anchor("调试断点单步执行"),
        _make_anchor("代码评审 Code Review"),
        _make_anchor("Stack Overflow 报错搜索"),
        _make_anchor("正则表达式匹配测试"),
        _make_anchor("Jupyter Notebook 数据分析"),
    ],
    "学习教育": [
        _make_anchor("高等数学课程视频"),
        _make_anchor("考研英语单词背诵"),
        _make_anchor("线性代数矩阵运算"),
        _make_anchor("物理力学实验报告"),
        _make_anchor("雅思听力真题练习"),
        _make_anchor("托福口语口试训练"),
        _make_anchor("Anki 闪卡复习"),
        _make_anchor("Coursera 在线课程"),
        _make_anchor("中国大学 MOOC 学习"),
        _make_anchor("网易云课堂视频回放"),
        _make_anchor("学习通在线测验"),
        _make_anchor("雨课堂课件 PPT"),
        _make_anchor("LeetCode 算法刷题"),
        _make_anchor("牛客网面试题库"),
        _make_anchor("化学元素周期表"),
        _make_anchor("中学物理公式整理"),
        _make_anchor("驾考科目一题库"),
        _make_anchor("有道词典英汉翻译"),
        _make_anchor("作业批改与讲解"),
        _make_anchor("期末考试复习笔记"),
    ],
    "影音娱乐": [
        _make_anchor("网易云音乐播放列表"),
        _make_anchor("QQ 音乐我的歌单"),
        _make_anchor("酷狗音乐曲库"),
        _make_anchor("Spotify 流媒体音乐"),
        _make_anchor("哔哩哔哩首页"),
        _make_anchor("B 站视频弹幕"),
        _make_anchor("B 站番剧追更"),
        _make_anchor("腾讯视频电视剧"),
        _make_anchor("爱奇艺综艺节目"),
        _make_anchor("优酷电影点播"),
        _make_anchor("抖音短视频"),
        _make_anchor("快手直播间"),
        _make_anchor("YouTube 视频推荐"),
        _make_anchor("Twitch 直播频道"),
        _make_anchor("PotPlayer 视频播放"),
        _make_anchor("VLC 媒体播放器"),
        _make_anchor("主播游戏直播"),
        _make_anchor("电影剪辑混剪"),
        _make_anchor("综艺节目精彩回顾"),
        _make_anchor("演唱会现场录像"),
    ],
    "社交通讯": [
        _make_anchor("微信"),
        _make_anchor("微信聊天消息"),
        _make_anchor("QQ"),
        _make_anchor("QQ 群聊通知"),
        _make_anchor("Discord 频道讨论"),
        _make_anchor("钉钉工作群消息"),
        _make_anchor("企业微信通讯"),
        _make_anchor("飞书会议邀请"),
        _make_anchor("Slack 团队频道"),
        _make_anchor("Microsoft Teams 会议"),
        _make_anchor("Telegram 私信"),
        _make_anchor("Outlook 邮件收件箱"),
        _make_anchor("Gmail 新邮件提醒"),
        _make_anchor("QQ 邮箱办公邮件"),
        _make_anchor("网易邮箱通知"),
        _make_anchor("短信验证码"),
        _make_anchor("微博私信会话"),
        _make_anchor("小红书消息中心"),
        _make_anchor("知乎站内私信"),
        _make_anchor("邮件回复转发"),
    ],
    "办公文档": [
        _make_anchor("文档1 - Microsoft Word"),
        _make_anchor("工作簿1 - Microsoft Excel"),
        _make_anchor("演示文稿1 - PowerPoint"),
        _make_anchor("WPS 文字"),
        _make_anchor("WPS 表格"),
        _make_anchor("WPS 演示"),
        _make_anchor("金山文档协作"),
        _make_anchor("腾讯文档在线编辑"),
        _make_anchor("石墨文档协作"),
        _make_anchor("飞书云文档"),
        _make_anchor("Notion 工作空间"),
        _make_anchor("Obsidian 笔记仓库"),
        _make_anchor("OneNote 笔记本"),
        _make_anchor("印象笔记 Evernote"),
        _make_anchor("Typora Markdown 编辑"),
        _make_anchor("PDF 阅读与批注"),
        _make_anchor("Adobe Acrobat PDF 编辑"),
        _make_anchor("会议纪要整理归档"),
        _make_anchor("合同审批流程"),
        _make_anchor("报销单填写提交"),
    ],
    "网页浏览": [
        _make_anchor("百度搜索结果"),
        _make_anchor("Google 搜索"),
        _make_anchor("Bing 搜索引擎"),
        _make_anchor("微博热搜话题"),
        _make_anchor("豆瓣评分书影音"),
        _make_anchor("淘宝商品详情"),
        _make_anchor("京东电商购物"),
        _make_anchor("拼多多优惠商品"),
        _make_anchor("天猫旗舰店"),
        _make_anchor("携程酒店预订"),
        _make_anchor("12306 火车票预订"),
        _make_anchor("美团外卖订餐"),
        _make_anchor("饿了么外卖"),
        _make_anchor("高德地图路线规划"),
        _make_anchor("BOSS 直聘招聘"),
        _make_anchor("拉勾招聘求职"),
        _make_anchor("贴吧讨论帖"),
        _make_anchor("天气预报查询"),
        _make_anchor("快递物流查询"),
        _make_anchor("银行网银转账"),
    ],
    "游戏": [
        # 应用名 / 游戏名
        _make_anchor("原神"),
        _make_anchor("崩坏：星穹铁道"),
        _make_anchor("绝区零"),
        _make_anchor("明日方舟"),
        _make_anchor("王者荣耀"),
        _make_anchor("和平精英"),
        _make_anchor("英雄联盟"),
        _make_anchor("League of Legends"),
        _make_anchor("Dota 2"),
        _make_anchor("CS2 竞技匹配"),
        _make_anchor("Valorant 战术射击"),
        _make_anchor("APEX 英雄战斗"),
        _make_anchor("永劫无间天选之人"),
        _make_anchor("Minecraft 生存模式"),
        _make_anchor("黑神话悟空"),
        # 平台 / 启动器
        _make_anchor("Steam 游戏库"),
        _make_anchor("Epic Games 启动器"),
        _make_anchor("Battle.net 战网"),
        _make_anchor("WeGame 平台"),
        # 活动
        _make_anchor("游戏排位赛对局"),
        _make_anchor("Boss 战副本攻略"),
        _make_anchor("游戏更新补丁下载"),
    ],
    "设计创作": [
        _make_anchor("Photoshop 图层编辑"),
        _make_anchor("Illustrator 矢量绘图"),
        _make_anchor("InDesign 排版印刷"),
        _make_anchor("Premiere Pro 视频剪辑"),
        _make_anchor("After Effects 视觉特效"),
        _make_anchor("DaVinci Resolve 调色"),
        _make_anchor("剪映视频编辑"),
        _make_anchor("Figma 界面原型设计"),
        _make_anchor("Sketch UI 设计"),
        _make_anchor("Adobe XD 交互原型"),
        _make_anchor("Blender 三维建模"),
        _make_anchor("Cinema 4D 渲染"),
        _make_anchor("Maya 角色动画"),
        _make_anchor("ZBrush 数字雕刻"),
        _make_anchor("Procreate 数字绘画"),
        _make_anchor("Krita 板绘"),
        _make_anchor("AutoCAD 工程制图"),
        _make_anchor("SketchUp 建筑建模"),
        _make_anchor("海报设计排版"),
        _make_anchor("LOGO 矢量设计"),
    ],
    "系统工具": [
        _make_anchor("Windows 设置"),
        _make_anchor("控制面板"),
        _make_anchor("任务管理器"),
        _make_anchor("文件资源管理器"),
        _make_anchor("PowerShell 命令行"),
        _make_anchor("命令提示符 cmd"),
        _make_anchor("Windows 终端"),
        _make_anchor("注册表编辑器"),
        _make_anchor("服务管理器 services.msc"),
        _make_anchor("事件查看器"),
        _make_anchor("磁盘管理工具"),
        _make_anchor("设备管理器"),
        _make_anchor("系统更新 Windows Update"),
        _make_anchor("驱动安装程序"),
        _make_anchor("杀毒软件扫描"),
        _make_anchor("7-Zip 压缩"),
        _make_anchor("WinRAR 解压"),
        _make_anchor("防火墙规则配置"),
        _make_anchor("蓝牙设备配对"),
        _make_anchor("剪贴板历史"),
    ],
    "阅读资讯": [
        _make_anchor("微信读书电子书"),
        _make_anchor("Kindle 阅读器"),
        _make_anchor("起点中文网小说"),
        _make_anchor("番茄小说阅读"),
        _make_anchor("得到 App 听书"),
        _make_anchor("知网论文下载"),
        _make_anchor("arXiv 预印本论文"),
        _make_anchor("Google Scholar 学术搜索"),
        _make_anchor("学术论文文献综述"),
        _make_anchor("Zotero 文献管理"),
        _make_anchor("RSS 订阅文章更新"),
        _make_anchor("今日头条新闻"),
        _make_anchor("网易新闻订阅"),
        _make_anchor("腾讯新闻热点"),
        _make_anchor("澎湃新闻深度报道"),
        _make_anchor("36氪科技资讯"),
        _make_anchor("CSDN 技术博客"),
        _make_anchor("掘金技术文章"),
        _make_anchor("少数派文章"),
        _make_anchor("Medium 英文博客"),
    ],
}


# ---------------------------------------------------------------------------
# Singleton text embedder for BGE-small-zh-v1.5
# ---------------------------------------------------------------------------

class TextEmbedder:
    """Singleton for the BAAI/bge-small-zh-v1.5 text embedding model."""

    _instance = None
    _model = None
    _tokenizer = None

    def __new__(cls):
        if cls._instance is None:
            cls._instance = super().__new__(cls)
        return cls._instance

    def initialize(self):
        """Load model & tokenizer (lazy, called once)."""
        if self._model is not None:
            return

        from transformers import AutoTokenizer, AutoModel

        model_path = os.environ.get("BGE_MODEL_PATH")
        if not model_path:
            model_path = os.path.join(
                os.environ.get("LOCALAPPDATA", os.path.expanduser("~")),
                "carbonPaper",
                "models",
                "bge-small-zh-v1.5",
            )

        from logging_config import log_model_loading
        log_model_loading("BGE-small-zh-v1.5")
        logger.info("Loading BGE-small-zh-v1.5 from %s ...", model_path)
        self._tokenizer = AutoTokenizer.from_pretrained(model_path)
        self._model = AutoModel.from_pretrained(model_path)
        self._model.eval()
        logger.info("BGE-small-zh-v1.5 loaded successfully (device=%s)", self._model.device)

    def encode(self, texts: List[str]) -> np.ndarray:
        """Batch-encode texts; returns (N, dim) L2-normalised numpy array."""
        self.initialize()
        import torch

        encoded = self._tokenizer(
            texts,
            padding=True,
            truncation=True,
            max_length=512,
            return_tensors="pt",
        )
        with torch.no_grad():
            out = self._model(**encoded)
            # CLS pooling (standard for BGE models)
            emb = out.last_hidden_state[:, 0, :]
            emb = torch.nn.functional.normalize(emb, p=2, dim=1)
        return emb.cpu().numpy()

    def encode_single(self, text: str) -> np.ndarray:
        """Encode a single text string; returns (dim,) vector."""
        return self.encode([text])[0]


# ---------------------------------------------------------------------------
# Classification service
# ---------------------------------------------------------------------------

class ClassificationService:
    """Anchor-based text classifier using **weighted** cosine similarity in BGE embedding space.

    Anchor format v2:
        Dict[str, List[Dict]]  where each dict has keys: text, source, weight, added_at
    Legacy format (auto-migrated):
        Dict[str, List[str]]
    """

    # Threshold above which we trust the title-only score
    TITLE_CONFIDENCE_THRESHOLD = 0.55
    # Below this the result is "未分类" (Unclassified)
    CLASSIFY_MIN_THRESHOLD = 0.38
    # Minimum cosine similarity for an anchor to participate in voting.
    # Prevents high-weight but semantically-unrelated anchors from dominating.
    RELEVANCE_FLOOR = 0.3
    # How many top anchors per category participate in scoring
    TOPK_PER_CATEGORY = 3
    # Local/global blending: global-base + local additive boost scaled by diversity
    LOCAL_BOOST_MONO = 0.15       # diversity = 1 (single category in local)
    LOCAL_BOOST_MODERATE = 0.30   # diversity = 2-3
    LOCAL_BOOST_DIVERSE = 0.50    # diversity >= 4
    # If the local channel already strongly agrees with the title-blended winner,
    # skip OCR fallback even when blended score is below TITLE_CONFIDENCE_THRESHOLD.
    LOCAL_VETO_THRESHOLD = 0.5

    # Known browser process names → regex to strip application suffix from titles.
    # Only these processes get title cleaning for global anchors/queries.
    # Note: \u200b (zero-width space) appears in real window titles between words.
    _ZWS = r'[\u200b\u200c\u200d\ufeff]*'  # zero-width chars that appear in real titles
    BROWSER_SUFFIX_PATTERNS: Dict[str, re.Pattern] = {
        "msedge.exe":   re.compile(rf'\s*[-–—]\s*(?:[\w\s]*[-–—]\s*)?Microsoft{_ZWS}\s*Edge.*$', re.IGNORECASE),
        "chrome.exe":   re.compile(rf'\s*[-–—]\s*(?:[\w\s]*[-–—]\s*)?Google{_ZWS}\s*Chrome.*$', re.IGNORECASE),
        "firefox.exe":  re.compile(rf'\s*[-–—]\s*(?:[\w\s]*[-–—]\s*)?Mozilla{_ZWS}\s*Firefox.*$', re.IGNORECASE),
        "brave.exe":    re.compile(rf'\s*[-–—]\s*(?:[\w\s]*[-–—]\s*)?Brave.*$', re.IGNORECASE),
        "opera.exe":    re.compile(rf'\s*[-–—]\s*(?:[\w\s]*[-–—]\s*)?Opera.*$', re.IGNORECASE),
        "vivaldi.exe":  re.compile(rf'\s*[-–—]\s*(?:[\w\s]*[-–—]\s*)?Vivaldi.*$', re.IGNORECASE),
    }
    # Generic fallback pattern that matches common browser suffixes regardless of process name
    _GENERIC_BROWSER_SUFFIX = re.compile(
        rf'\s*[-–—]\s*(?:[\w\s]*[-–—]\s*)?'
        rf'(?:Microsoft{_ZWS}\s*Edge|Google{_ZWS}\s*Chrome|Mozilla{_ZWS}\s*Firefox|Brave|Opera|Vivaldi)'
        rf'(?:\s*(?:Beta|Dev|Canary|Nightly))?\s*$',
        re.IGNORECASE,
    )

    def __init__(self, anchors_path: Optional[str] = None):
        self.embedder = TextEmbedder()
        self.anchors_path = anchors_path or os.path.join(
            os.environ.get("LOCALAPPDATA", os.path.expanduser("~")),
            "carbonPaper",
            "data",
            "anchors.json",
        )

        # Structured anchors: { category: [ {text, source, weight, added_at}, ... ] }
        self.anchors: Dict[str, List[Dict[str, Any]]] = {}
        # Pre-computed embeddings
        self.anchor_matrix: Optional[np.ndarray] = None  # (total_anchors, dim)
        self.anchor_weights: Optional[np.ndarray] = None  # (total_anchors,)
        self.anchor_labels: List[str] = []  # category name per row
        self.anchor_scopes: List[str] = []  # scope per row (global/local)
        self.anchor_process_names: List[str] = []  # process name per row (lower-cased)
        self.category_names: List[str] = []
        self._index_built = False

        self._load_anchors()

    # ---- persistence --------------------------------------------------

    @staticmethod
    def _migrate_anchors(raw: Dict) -> Dict[str, List[Dict[str, Any]]]:
        """Auto-convert legacy ``Dict[str, List[str]]`` to structured format."""
        migrated: Dict[str, List[Dict[str, Any]]] = {}
        for cat, entries in raw.items():
            if not isinstance(entries, list):
                continue
            anchors_list: List[Dict[str, Any]] = []
            for entry in entries:
                if isinstance(entry, str):
                    # Legacy plain-text anchor → convert
                    anchors_list.append(_make_anchor(entry, source="default", weight=WEIGHT_DEFAULT))
                elif isinstance(entry, dict) and "text" in entry:
                    # Already structured
                    item = dict(entry)
                    item.setdefault("source", "default")
                    item.setdefault("weight", WEIGHT_DEFAULT)
                    item.setdefault("scope", "global")
                    item.setdefault("process_name", None)
                    item.setdefault("added_at", time.strftime("%Y-%m-%dT%H:%M:%S"))
                    anchors_list.append(item)
                # else: skip malformed entries
            migrated[cat] = anchors_list
        return migrated

    def _load_anchors(self):
        if os.path.exists(self.anchors_path):
            try:
                with open(self.anchors_path, "r", encoding="utf-8") as f:
                    raw = json.load(f)
                self.anchors = self._migrate_anchors(raw)
                self._ensure_default_global_anchors()
                # Re-save in case migration converted legacy format
                self._save_anchors()
                logger.info(
                    "Loaded %d categories from %s",
                    len(self.anchors),
                    self.anchors_path,
                )
            except Exception as exc:
                logger.warning("Failed to load anchors file, using defaults: %s", exc)
                self.anchors = {k: list(v) for k, v in DEFAULT_ANCHORS.items()}
                self._save_anchors()
        else:
            self.anchors = {k: list(v) for k, v in DEFAULT_ANCHORS.items()}
            self._save_anchors()
            logger.info("Created default anchors at %s", self.anchors_path)

    def _ensure_default_global_anchors(self):
        """Backfill missing default categories/anchors to preserve global cold-start capability."""
        for cat, default_list in DEFAULT_ANCHORS.items():
            existing = self.anchors.get(cat)
            if not isinstance(existing, list) or len(existing) == 0:
                self.anchors[cat] = [dict(a) for a in default_list]
                continue

            existing_texts = set()
            for a in existing:
                if isinstance(a, dict):
                    t = str(a.get("text", "")).strip()
                    if t:
                        existing_texts.add(t)
            for da in default_list:
                dt = str(da.get("text", "")).strip()
                if dt and dt not in existing_texts:
                    self.anchors[cat].append(dict(da))
                    existing_texts.add(dt)

    def _save_anchors(self):
        os.makedirs(os.path.dirname(self.anchors_path), exist_ok=True)
        with open(self.anchors_path, "w", encoding="utf-8") as f:
            json.dump(self.anchors, f, ensure_ascii=False, indent=2)

    # ---- index building -----------------------------------------------

    def _ensure_index(self):
        if not self._index_built:
            self._build_index()
            self._index_built = True

    def _build_index(self):
        """Pre-compute embeddings and weights for all anchors."""
        all_texts: List[str] = []
        labels: List[str] = []
        weights: List[float] = []
        scopes: List[str] = []
        process_names: List[str] = []
        for cat, anchor_list in self.anchors.items():
            for anchor in anchor_list:
                text = anchor.get("text", "") if isinstance(anchor, dict) else str(anchor)
                weight = anchor.get("weight", WEIGHT_DEFAULT) if isinstance(anchor, dict) else WEIGHT_DEFAULT
                scope = anchor.get("scope", "global") if isinstance(anchor, dict) else "global"
                process_name = anchor.get("process_name") if isinstance(anchor, dict) else None
                if not text.strip():
                    continue
                all_texts.append(text)
                labels.append(cat)
                weights.append(float(weight))
                scopes.append(str(scope or "global").lower())
                process_names.append(str(process_name or "").strip().lower())

        if not all_texts:
            self.anchor_matrix = np.zeros((0, 512))
            self.anchor_weights = np.zeros(0)
            self.anchor_labels = []
            self.anchor_scopes = []
            self.anchor_process_names = []
            self.category_names = []
            return

        self.anchor_matrix = self.embedder.encode(all_texts)  # (N, dim)
        self.anchor_weights = np.array(weights, dtype=np.float32)  # (N,)
        self.anchor_labels = labels
        self.anchor_scopes = scopes
        self.anchor_process_names = process_names
        self.category_names = list(self.anchors.keys())
        logger.info(
            "Built anchor index: %d anchors across %d categories",
            len(all_texts),
            len(self.category_names),
        )

    @staticmethod
    def _is_informative_text(text: str, min_len: int = 8) -> bool:
        """Heuristic gate for global learning (avoid short/fixed-title pollution)."""
        if not text:
            return False
        s = re.sub(r"\s+", " ", text).strip()
        if len(s) < min_len:
            return False
        alnum_count = sum(1 for ch in s if ch.isalnum())
        unique_chars = len(set(s))
        if alnum_count < max(4, min_len // 2):
            return False
        if unique_chars < 4:
            return False
        if re.fullmatch(r"[\W_]+", s):
            return False
        return True

    @staticmethod
    def _clean_ocr_text(ocr_text: str, max_tokens: int = 24, max_chars: int = 200) -> str:
        """Extract a cleaner OCR snippet for learning anchors."""
        if not ocr_text:
            return ""
        text = re.sub(r"\s+", " ", ocr_text).strip()
        if not text:
            return ""
        tokens = re.split(r"\s+", text)
        filtered: List[str] = []
        for t in tokens:
            t = t.strip()
            if not t:
                continue
            if len(t) <= 1:
                continue
            if re.fullmatch(r"[\d:./\\-]+", t):
                continue
            filtered.append(t)
            if len(filtered) >= max_tokens:
                break
        out = " ".join(filtered).strip()
        return out[:max_chars]

    @classmethod
    def _strip_app_suffix(cls, text: str, process_name: str = "") -> str:
        """Strip browser application suffix from window titles for cleaner embeddings.

        Only acts on known browser processes.  For unknown processes the original
        text is returned unchanged — this avoids accidentally removing meaningful
        content from non-browser window titles that happen to contain " - ".

        Examples:
            "bilibili视频 - 个人 - Microsoft Edge Beta"  →  "bilibili视频"
            "Python教程 - Google Chrome"                 →  "Python教程"
            "记事本 - foo.txt"                           →  "记事本 - foo.txt"  (not a browser)
        """
        if not text or not text.strip():
            return text

        proc = (process_name or "").strip().lower()
        pattern = cls.BROWSER_SUFFIX_PATTERNS.get(proc)

        if pattern is None:
            # Not a known browser → return as-is
            return text

        cleaned = pattern.sub("", text).strip()
        # Safety: if stripping removed too much, keep original
        if len(cleaned) < 2:
            return text
        return cleaned

    @classmethod
    def _strip_browser_suffix_generic(cls, text: str) -> str:
        """Strip browser suffix using generic pattern (process-agnostic).

        Useful for cleaning existing global anchor texts where process_name
        is not available.
        """
        if not text:
            return text
        cleaned = cls._GENERIC_BROWSER_SUFFIX.sub("", text).strip()
        if len(cleaned) < 2:
            return text
        return cleaned

    @staticmethod
    def _apply_process_prior(
        cat_scores: Dict[str, float],
        process_name: str,
    ) -> Dict[str, float]:
        """Add a small fixed bonus to the category prior-mapped by ``process_name``.

        Returns a *new* dict — does not mutate the input.  No-op if the process
        is not in PROCESS_CATEGORY_PRIOR or the mapped category is missing from
        the scores.
        """
        proc = (process_name or "").strip().lower()
        prior_cat = PROCESS_CATEGORY_PRIOR.get(proc)
        if not prior_cat or prior_cat not in cat_scores:
            return cat_scores
        out = dict(cat_scores)
        out[prior_cat] = out[prior_cat] + PROCESS_PRIOR_BONUS
        return out

    @staticmethod
    def _match_scope(
        anchor_scope: str,
        anchor_process: str,
        query_process: str,
        channel: str,
    ) -> bool:
        """Check whether an anchor should participate in a scoring channel."""
        scope = (anchor_scope or "global").lower()
        if channel == "global":
            return scope != "local"
        if channel == "local":
            if scope != "local":
                return False
            if not query_process:
                return False
            return anchor_process == query_process
        return True

    # ---- classification -----------------------------------------------

    def _score_embedding(
        self,
        query_emb: np.ndarray,
        process_name: str = "",
        channel: str = "all",
        include_debug: bool = False,
    ) -> Dict[str, float]:
        """Score a single query embedding against all anchors."""
        self._ensure_index()
        BONUS_FACTOR = 0.05  # each +1.0 weight adds this much to the raw cosine

        raw_scores = self.anchor_matrix @ query_emb  # (N,)

        query_process = (process_name or "").strip().lower()

        # Per-category: collect (cosine, weight, idx) for qualifying anchors
        cat_candidates: Dict[str, list] = {}
        for i, label in enumerate(self.anchor_labels):
            cos = float(raw_scores[i])
            if cos < self.RELEVANCE_FLOOR:
                continue
            if not self._match_scope(
                self.anchor_scopes[i] if i < len(self.anchor_scopes) else "global",
                self.anchor_process_names[i] if i < len(self.anchor_process_names) else "",
                query_process,
                channel,
            ):
                continue
            w = float(self.anchor_weights[i])
            cat_candidates.setdefault(label, []).append((cos, w, i))

        cat_scores: Dict[str, float] = {}
        debug_hits: Dict[str, List[Dict[str, Any]]] = {}
        for cat, pairs in cat_candidates.items():
            # Sort by raw cosine descending, take top K
            pairs.sort(key=lambda p: p[0], reverse=True)
            top = pairs[: self.TOPK_PER_CATEGORY]
            # Effective score = best_cosine + small bonus from weight
            best_cos, best_w, _ = top[0]
            effective = best_cos + BONUS_FACTOR * (best_w - 1.0)
            cat_scores[cat] = effective
            if include_debug:
                hit_list: List[Dict[str, Any]] = []
                for cos, w, idx in top:
                    anchor = self.anchors.get(cat, [])
                    anchor_text = ""
                    if isinstance(anchor, list):
                        # Recover text via index fallback from row metadata
                        row_text = None
                        if idx < len(self.anchor_labels):
                            # idx is global index in flattened anchors; text retrieval from flattened list isn't direct,
                            # so we keep short metadata only
                            row_text = None
                        anchor_text = row_text or ""
                    hit_list.append({
                        "cosine": round(float(cos), 4),
                        "weight": float(w),
                        "scope": self.anchor_scopes[idx] if idx < len(self.anchor_scopes) else "global",
                        "process_name": self.anchor_process_names[idx] if idx < len(self.anchor_process_names) else "",
                        "text": anchor_text,
                    })
                debug_hits[cat] = hit_list

        # Fill missing categories with 0 so downstream logic doesn't KeyError
        for cat in self.category_names:
            cat_scores.setdefault(cat, 0.0)

        if include_debug:
            cat_scores["__debug_hits__"] = debug_hits

        return cat_scores

    @staticmethod
    def _blend_channel_scores(
        local_scores: Dict[str, float],
        global_scores: Dict[str, float],
        category_names: List[str],
    ) -> Dict[str, float]:
        """Blend local/global channels: global-base + local additive boost.

        The global score is always the baseline.  Local evidence provides an
        *additive* boost whose magnitude scales with local diversity:

        - ``diversity = 0`` → pure global (no local data for this process)
        - ``diversity = 1`` → ``global + 0.15 × local`` (single-category local
          has weak discriminative power but still gives a mild signal)
        - ``diversity ∈ [2, 3]`` → ``global + 0.30 × local``
        - ``diversity ≥ 4`` → ``global + 0.50 × local``

        Invariant: ``blended[cat] ≥ global[cat]`` — local evidence never *lowers*
        a category's ranking compared to what global alone would give.
        """
        blended: Dict[str, float] = {}

        local_nonzero_cats = [
            c for c in category_names if local_scores.get(c, 0.0) > 0.0
        ] if local_scores else []
        local_diversity = len(local_nonzero_cats)

        # Select boost factor based on diversity
        if local_diversity <= 0:
            boost = 0.0
        elif local_diversity == 1:
            boost = ClassificationService.LOCAL_BOOST_MONO
        elif local_diversity <= 3:
            boost = ClassificationService.LOCAL_BOOST_MODERATE
        else:
            boost = ClassificationService.LOCAL_BOOST_DIVERSE

        for cat in category_names:
            gs = float(global_scores.get(cat, 0.0))
            ls = float(local_scores.get(cat, 0.0))
            blended[cat] = gs + boost * ls

        return blended

    def classify(
        self,
        title: str,
        ocr_text: str = "",
        process_name: str = "",
        title_weight: float = 0.8,
    ) -> Tuple[str, float]:
        """Classify a screenshot by title (and optionally OCR text).

        Scoring uses **top-K weighted voting** with a relevance floor:
            1. Compute cosine similarity of query against every anchor.
            2. Discard anchors below RELEVANCE_FLOOR (prevents weight-pollution).
            3. Per category, take best cosine and apply additive weight bonus.

        Returns:
            (category_name, confidence) — ``"未分类"`` when below threshold.
        """
        self._ensure_index()
        if self.anchor_matrix is None or len(self.anchor_matrix) == 0:
            return ("未分类", 0.0)

        if not title or not title.strip():
            if not ocr_text or not ocr_text.strip():
                return ("未分类", 0.0)
            title = ocr_text[:200]

        clean_title = self._strip_app_suffix(title, process_name)
        title_emb = self.embedder.encode_single(clean_title)  # (dim,)
        title_local = self._score_embedding(title_emb, process_name=process_name, channel="local")
        title_global = self._score_embedding(title_emb, process_name=process_name, channel="global")
        cat_scores = self._blend_channel_scores(title_local, title_global, self.category_names)

        best_cat = max(cat_scores, key=cat_scores.get)
        best_score = cat_scores[best_cat]

        # OCR fallback gate: skip OCR blending when the local channel already
        # strongly agrees with the title-blended winner — prevents an OCR-heavy
        # signal from overturning a confident process-scoped judgment.
        if title_local:
            local_best_cat = max(title_local, key=title_local.get)
            local_best_score = float(title_local.get(local_best_cat, 0.0))
        else:
            local_best_cat = best_cat
            local_best_score = 0.0
        local_agrees_strongly = (
            local_best_score >= self.LOCAL_VETO_THRESHOLD
            and local_best_cat == best_cat
        )

        # If title confidence is low and OCR text is available, blend
        if (
            best_score < self.TITLE_CONFIDENCE_THRESHOLD
            and not local_agrees_strongly
            and ocr_text and ocr_text.strip()
        ):
            ocr_snippet = self._clean_ocr_text(ocr_text)[:200]
            if not ocr_snippet:
                ocr_snippet = ocr_text[:200]
            ocr_emb = self.embedder.encode_single(ocr_snippet)
            ocr_local = self._score_embedding(ocr_emb, process_name=process_name, channel="local")
            ocr_global = self._score_embedding(ocr_emb, process_name=process_name, channel="global")
            ocr_cat = self._blend_channel_scores(ocr_local, ocr_global, self.category_names)

            for cat in cat_scores:
                ocr_s = ocr_cat.get(cat, 0.0)
                cat_scores[cat] = title_weight * cat_scores[cat] + (1 - title_weight) * ocr_s

        # Apply process→category prior (additive bonus on the final blended score)
        cat_scores = self._apply_process_prior(cat_scores, process_name)

        best_cat = max(cat_scores, key=cat_scores.get)
        best_score = cat_scores[best_cat]

        if best_score < self.CLASSIFY_MIN_THRESHOLD:
            return ("未分类", best_score)

        return (best_cat, best_score)

    def classify_debug(
        self,
        title: str,
        ocr_text: str = "",
        process_name: str = "",
        title_weight: float = 0.8,
    ) -> Dict[str, Any]:
        """Return detailed channel scores for diagnostics."""
        self._ensure_index()
        if self.anchor_matrix is None or len(self.anchor_matrix) == 0:
            return {
                "category": "未分类",
                "category_confidence": 0.0,
                "reason": "empty_anchor_index",
            }

        q_title = (title or "").strip()
        q_ocr = (ocr_text or "").strip()
        if not q_title and not q_ocr:
            return {
                "category": "未分类",
                "category_confidence": 0.0,
                "reason": "empty_input",
            }
        if not q_title:
            q_title = q_ocr[:200]

        # Strip browser suffix for global channel
        clean_title = self._strip_app_suffix(q_title, process_name)

        title_emb_full = self.embedder.encode_single(q_title)
        title_emb_clean = (
            self.embedder.encode_single(clean_title) if clean_title != q_title else title_emb_full
        )

        title_local = self._score_embedding(title_emb_full, process_name=process_name, channel="local")
        title_global = self._score_embedding(title_emb_clean, process_name=process_name, channel="global")
        blended_title = self._blend_channel_scores(title_local, title_global, self.category_names)

        best_cat = max(blended_title, key=blended_title.get) if blended_title else "未分类"
        best_score = blended_title.get(best_cat, 0.0) if blended_title else 0.0

        # OCR fallback gate — see classify() for rationale.
        if title_local:
            local_best_cat = max(title_local, key=title_local.get)
            local_best_score = float(title_local.get(local_best_cat, 0.0))
        else:
            local_best_cat = best_cat
            local_best_score = 0.0
        local_agrees_strongly = (
            local_best_score >= self.LOCAL_VETO_THRESHOLD
            and local_best_cat == best_cat
        )

        used_ocr = False
        local_veto_active = False
        if best_score < self.TITLE_CONFIDENCE_THRESHOLD and q_ocr:
            if local_agrees_strongly:
                local_veto_active = True
            else:
                used_ocr = True
                ocr_snippet = self._clean_ocr_text(q_ocr)[:200] or q_ocr[:200]
                ocr_emb = self.embedder.encode_single(ocr_snippet)
                ocr_local = self._score_embedding(ocr_emb, process_name=process_name, channel="local")
                ocr_global = self._score_embedding(ocr_emb, process_name=process_name, channel="global")
                blended_ocr = self._blend_channel_scores(ocr_local, ocr_global, self.category_names)
                for cat in self.category_names:
                    blended_title[cat] = title_weight * blended_title.get(cat, 0.0) + (1 - title_weight) * blended_ocr.get(cat, 0.0)

        # Apply process→category prior (additive bonus on the final blended score)
        blended_title = self._apply_process_prior(blended_title, process_name)

        best_cat = max(blended_title, key=blended_title.get)
        best_score = blended_title[best_cat]

        if best_score < self.CLASSIFY_MIN_THRESHOLD:
            best_cat = "未分类"

        sorted_scores = sorted(blended_title.items(), key=lambda kv: kv[1], reverse=True)[:5]

        # Compute local channel diversity for diagnostics
        local_nonzero = [c for c in self.category_names if title_local.get(c, 0.0) > 0.0]
        local_top = sorted(title_local.items(), key=lambda kv: kv[1], reverse=True)[:3]
        global_top = sorted(title_global.items(), key=lambda kv: kv[1], reverse=True)[:3]

        prior_cat = PROCESS_CATEGORY_PRIOR.get((process_name or "").strip().lower())

        return {
            "category": best_cat,
            "category_confidence": round(float(best_score), 4),
            "used_ocr": used_ocr,
            "local_veto_active": local_veto_active,
            "process_prior_applied": prior_cat,
            "process_name": process_name,
            "cleaned_title": clean_title,
            "top_scores": [{"category": k, "score": round(float(v), 4)} for k, v in sorted_scores],
            "local_diversity": len(local_nonzero),
            "local_categories": local_nonzero,
            "local_top": [{"category": k, "score": round(float(v), 4)} for k, v in local_top],
            "global_top": [{"category": k, "score": round(float(v), 4)} for k, v in global_top],
        }

    # ---- semantic deduplication ----------------------------------------

    def _is_duplicate(
        self,
        category: str,
        text_emb: np.ndarray,
        scope: str = "global",
        process_name: str = "",
    ) -> bool:
        """Check if text_emb is semantically duplicate with any existing anchor in `category`.

        Returns True if cosine similarity > DEDUP_COSINE_THRESHOLD with any existing anchor.
        """
        self._ensure_index()
        if self.anchor_matrix is None or len(self.anchor_matrix) == 0:
            return False

        # Compute cosine similarity against all anchors in this category
        p = (process_name or "").strip().lower()
        for i, label in enumerate(self.anchor_labels):
            if label != category:
                continue
            if scope == "local":
                if (self.anchor_scopes[i] if i < len(self.anchor_scopes) else "global") != "local":
                    continue
                if (self.anchor_process_names[i] if i < len(self.anchor_process_names) else "") != p:
                    continue
            elif scope == "global":
                if (self.anchor_scopes[i] if i < len(self.anchor_scopes) else "global") == "local":
                    continue
            cos_sim = float(self.anchor_matrix[i] @ text_emb)
            if cos_sim > DEDUP_COSINE_THRESHOLD:
                return True
        return False

    # ---- anchor management (upgraded) ----------------------------------

    def add_anchor(
        self,
        category: str,
        title: str,
        ocr_text: str = "",
        old_category: Optional[str] = None,
        process_name: str = "",
    ) -> Dict[str, Any]:
        """Add anchor(s) from user manual classification with full learning logic.

        1. Negative feedback: remove title anchor from old_category if different.
        2. Add title as user_feedback anchor (with semantic dedup).
        3. If OCR text is long enough and different enough from title, also add OCR anchor.

        Returns dict with action summary.
        """
        result: Dict[str, Any] = {
            "title_local_added": False,
            "title_global_added": False,
            "title_local_dedup": False,
            "title_global_dedup": False,
            "ocr_local_added": False,
            "ocr_global_added": False,
            "ocr_local_dedup": False,
            "ocr_global_dedup": False,
            "negative_removed": False,
        }

        if not title or not title.strip():
            return result

        # --- 1. Negative feedback: remove from old category ---
        process_name_norm = (process_name or "").strip()
        should_add_local = bool(process_name_norm)

        if old_category and old_category != category and old_category != "未分类":
            removed = False
            if should_add_local:
                removed = self._remove_anchor_by_text(
                    old_category,
                    title,
                    scope="local",
                    process_name=process_name_norm,
                )
            if not removed:
                removed = self._remove_anchor_by_text(old_category, title, scope="global")
            result["negative_removed"] = removed
            if removed:
                logger.info(
                    "Negative feedback: removed '%s' from category '%s'",
                    title[:60], old_category,
                )

        # --- 2. Add title anchor(s) with dedup ---
        title_emb = self.embedder.encode_single(title)

        # For global anchors, strip browser suffix to avoid cross-content pollution
        clean_title = self._strip_app_suffix(title, process_name_norm)
        should_add_global = self._is_informative_text(clean_title)
        # If cleaning changed the text, compute a separate embedding for global dedup
        clean_title_emb = self.embedder.encode_single(clean_title) if clean_title != title else title_emb

        if should_add_local:
            if self._is_duplicate(category, title_emb, scope="local", process_name=process_name_norm):
                result["title_local_dedup"] = True
            else:
                if category not in self.anchors:
                    self.anchors[category] = []
                self.anchors[category].append(
                    _make_anchor(
                        title,
                        source="user_feedback",
                        weight=WEIGHT_USER_FEEDBACK,
                        scope="local",
                        process_name=process_name_norm,
                    )
                )
                result["title_local_added"] = True

        if should_add_global:
            if self._is_duplicate(category, clean_title_emb, scope="global"):
                result["title_global_dedup"] = True
            else:
                if category not in self.anchors:
                    self.anchors[category] = []
                self.anchors[category].append(
                    _make_anchor(
                        clean_title,
                        source="user_feedback",
                        weight=WEIGHT_USER_FEEDBACK,
                        scope="global",
                    )
                )
                result["title_global_added"] = True

        # --- 3. OCR auxiliary anchor ---
        # NOTE: OCR feedback is recorded as a *local* (process-scoped) anchor only.
        # Global OCR writes were intentionally removed — OCR content is highly
        # process-specific (chat UI, app navigation, etc.) and writing it as a
        # global anchor reliably caused cross-category embedding pollution.
        if ocr_text and len(ocr_text.strip()) >= OCR_MIN_LENGTH:
            ocr_snippet = self._clean_ocr_text(ocr_text)[:200]
            if not ocr_snippet:
                ocr_snippet = ocr_text.strip()[:200]
            ocr_emb = self.embedder.encode_single(ocr_snippet)

            # Only add if OCR is sufficiently different from title
            title_ocr_cos = float(title_emb @ ocr_emb)
            if title_ocr_cos < OCR_DIVERSITY_THRESHOLD:
                if should_add_local:
                    if self._is_duplicate(category, ocr_emb, scope="local", process_name=process_name_norm):
                        result["ocr_local_dedup"] = True
                    else:
                        if category not in self.anchors:
                            self.anchors[category] = []
                        self.anchors[category].append(
                            _make_anchor(
                                ocr_snippet,
                                source="ocr_feedback",
                                weight=WEIGHT_OCR_FEEDBACK,
                                scope="local",
                                process_name=process_name_norm,
                            )
                        )
                        result["ocr_local_added"] = True

        self._build_index()
        self._save_anchors()

        logger.info(
            "Anchor learning complete for '%s': title_local=%s, title_global=%s, "
            "ocr_local=%s, ocr_global=%s, negative_removed=%s (total=%d)",
            category, result["title_local_added"], result["title_global_added"],
            result["ocr_local_added"], result["ocr_global_added"], result["negative_removed"],
            len(self.anchor_labels),
        )
        return result

    def _remove_anchor_by_text(
        self,
        category: str,
        text: str,
        scope: Optional[str] = None,
        process_name: str = "",
    ) -> bool:
        """Remove anchor(s) matching the given text from a category."""
        if category not in self.anchors:
            return False

        scope_norm = (scope or "").strip().lower()
        process_norm = (process_name or "").strip().lower()

        def _keep(anchor: Any) -> bool:
            anchor_text = anchor.get("text", "") if isinstance(anchor, dict) else str(anchor)
            if anchor_text != text:
                return True

            if not isinstance(anchor, dict):
                return scope_norm != ""  # legacy anchors only removable when scope not requested

            if not scope_norm:
                return False

            anchor_scope = str(anchor.get("scope", "global")).lower()
            if anchor_scope != scope_norm:
                return True

            if scope_norm == "local":
                anchor_process = str(anchor.get("process_name") or "").strip().lower()
                return anchor_process != process_norm

            return False

        original_len = len(self.anchors[category])
        self.anchors[category] = [a for a in self.anchors[category] if _keep(a)]

        if len(self.anchors[category]) == original_len:
            return False  # nothing removed

        if not self.anchors[category]:
            del self.anchors[category]

        self._save_anchors()
        self._build_index()
        return True

    def remove_anchor(self, category: str, text: str) -> bool:
        """Remove an anchor text from a category (public API)."""
        return self._remove_anchor_by_text(category, text)

    def remove_local_anchors_by_process(self, category: str, process_name: str) -> int:
        """Remove all local anchors in a category bound to a process name."""
        if category not in self.anchors:
            return 0

        process_norm = (process_name or "").strip().lower()
        if not process_norm:
            return 0

        before = len(self.anchors[category])
        self.anchors[category] = [
            a for a in self.anchors[category]
            if not (
                isinstance(a, dict)
                and str(a.get("scope", "global")).lower() == "local"
                and str(a.get("process_name") or "").strip().lower() == process_norm
            )
        ]
        removed = before - len(self.anchors[category])
        if removed > 0:
            if not self.anchors[category]:
                del self.anchors[category]
            self._save_anchors()
            self._build_index()
        return removed

    def get_categories(self) -> List[str]:
        """Return the list of known category names."""
        return list(self.anchors.keys())

    def get_anchors(self) -> Dict[str, List[Dict[str, Any]]]:
        """Return a copy of all anchors (structured format)."""
        return {k: list(v) for k, v in self.anchors.items()}
