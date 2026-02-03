import React, { useState } from 'react';
import { Heart, RefreshCw, Github, User, CheckCircle2 } from 'lucide-react';
import { APP_VERSION } from '../lib/version';

export function About() {
    const [checking, setChecking] = useState(false);
    const [upToDate, setUpToDate] = useState(false);

    const handleCheckUpdate = () => {
        setChecking(true);
        // Mock check
        setTimeout(() => {
            setChecking(false);
            setUpToDate(true);
        }, 1500);
    };

    return (
        <div className="flex w-full h-full gap-8 p-8 overflow-hidden bg-ide-bg text-ide-text select-none">
            {/* Left Column: Project Info */}
            <div className="flex-1 flex flex-col min-w-0 pt-4">

                {/* Header Section: Logo & Title */}
                <div className="flex items-center gap-6 mb-8">
                    <div className="relative w-24 h-24 shrink-0 flex items-center justify-center bg-gradient-to-br from-ide-panel to-ide-bg rounded-[2rem] shadow-xl border border-ide-border group cursor-default">
                        {/* Logo Placeholder */}
                        <div className="absolute inset-0 bg-ide-accent/5 rounded-[2rem] transform rotate-3 group-hover:rotate-6 transition-transform duration-500"></div>
                        <svg className="w-12 h-12 text-ide-accent relative z-10 drop-shadow-lg" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
                            <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
                            <polyline points="14 2 14 8 20 8" />
                            <line x1="16" y1="13" x2="8" y2="13" />
                            <line x1="16" y1="17" x2="8" y2="17" />
                            <polyline points="10 9 9 9 8 9" />
                        </svg>
                    </div>

                    <div className="flex flex-col items-start">
                        <h1 className="text-4xl font-bold text-ide-text tracking-tight mb-3">CarbonPaper - 复写纸</h1>
                        <span className="px-3 py-1 rounded-full bg-ide-panel border border-ide-border text-xs font-mono text-ide-muted">
                            {APP_VERSION}
                        </span>
                    </div>
                </div>

                {/* Scrollable Description */}
                <div className="flex-1 overflow-y-auto pr-6 text-ide-muted leading-relaxed text-sm space-y-4">
                    <h3 className="text-ide-text font-semibold text-lg">关于项目</h3>
                    <p>
                        复写纸（carbonpaper）是一款开源的屏幕文字捕捉与智能检索工具，旨在帮助用户高效地记录和查找屏幕上的文字内容。
                        通过集成本地的OCR技术和语义搜索算法，复写纸能够实时捕捉屏幕文字，并将其转换为可搜索的文本数据。
                    </p>
                    <p>
                        该项目目前处于早期开发技术性验证阶段。仅少量分发。如遇到问题，请直接联系作者。
                    </p>

                    <h3 className="text-ide-text font-semibold text-lg pt-4">您的所有数据都本地处理</h3>
                    <p>
                        所有处理均在您的设备本地进行。OCR、向量嵌入和数据库存储均为100%离线。
                        您的数据在没有经过您的授权的前提下绝不会离开您的设备。
                    </p>
                    <h3 className="text-ide-text font-semibold text-lg pt-4">有关诊断遥测数据的说明</h3>
                    <p>
                        应用处于技术验证阶段，会收集一些基本的诊断遥测数据以帮助改进应用。这些数据包括但不限于：
                        应用日志、性能指标和使用统计信息。这些数据均为匿名收集，<div style={{display: 'inline', fontWeight: 'bold'}}>绝对不包含任何个人身份信息和OCR，向量以及数据库数据。</div>
                        您可以在设置中选择关闭诊断数据收集功能。
                    </p>

                    <h3 className="text-ide-text font-semibold text-lg pt-4">核心功能</h3>
                    <ul className="list-disc list-inside space-y-2 pl-2">
                        <li>实时屏幕OCR</li>
                        <li>使用向量嵌入的语义搜索</li>
                        <li>历史上下文时间线视图</li>
                        <li>隐私过滤器（隐身模式检测）</li>
                        <li>低资源占用</li>
                    </ul>

                    <h3 className="text-ide-text font-semibold text-lg pt-4">目前已知的问题</h3>
                    <ul className="list-disc list-inside space-y-2 pl-2">
                        <li>OCR识别准确率有待提升。默认采用的低分辨率OCR方案虽然可以节省不少性能，但是准确率比较堪忧。</li>
                        <li>启动python子服务存在效率不高的问题。</li>
                        <li>使用自然语言搜索时，可能出现不准确的结果。</li>
                        <li>在用户焦点处于经设置不可截取的窗口时，会导致时间轴错误显示为长时间停留在上一个焦点。</li>
                        <li>某些情况下（如快速切换焦点，画面较为复杂），可能导致基于OCR的关键词忽略出现错误，因而错误地截取隐私窗口。</li>
                        <li>不支持删除历史记录中的条目。</li>
                    </ul>
                </div>
            </div>

            {/* Right Column: Cards */}
            <div className="w-80 flex flex-col gap-5 shrink-0">

                {/* Contributors Card */}
                <div className="bg-ide-panel/50 border border-ide-border rounded-2xl p-6 backdrop-blur-sm">
                    <div className="flex items-center gap-2 mb-4">
                        <div className="p-2 rounded-lg bg-ide-bg border border-ide-border">
                            <Github className="w-4 h-4 text-ide-text" />
                        </div>
                        <h3 className="font-semibold text-ide-text">Contributors</h3>
                    </div>
                    <div className="flex flex-wrap gap-3">
                        {[1, 2, 3].map((i) => (
                            <div key={i} className="w-10 h-10 rounded-full bg-ide-bg border border-ide-border flex items-center justify-center text-ide-muted hover:border-ide-accent hover:text-ide-accent transition-all cursor-pointer transform hover:-translate-y-0.5">
                                <User className="w-5 h-5" />
                            </div>
                        ))}
                        <button className="h-10 px-4 rounded-full bg-ide-bg border border-dashed border-ide-border text-xs text-ide-muted hover:text-ide-text hover:border-ide-accent hover:bg-ide-accent/5 transition-all flex items-center gap-2">
                            + Join
                        </button>
                    </div>
                </div>

                {/* Update Check Card */}
                <div className="bg-ide-panel/50 border border-ide-border rounded-2xl p-6 backdrop-blur-sm flex items-center justify-between group">
                    <div>
                        <h3 className="font-semibold text-ide-text mb-1">Check Updates</h3>
                        <p className="text-xs text-ide-muted">
                            {upToDate
                                ? "Currently on the latest version"
                                : "New features might be available"}
                        </p>
                    </div>

                    <button
                        onClick={handleCheckUpdate}
                        disabled={checking || upToDate}
                        className={`
                    h-10 px-4 rounded-lg text-sm font-medium transition-all flex items-center gap-2 shadow-sm
                    ${upToDate
                                ? 'bg-emerald-500/10 text-emerald-500 cursor-default border border-emerald-500/20'
                                : 'bg-ide-bg border border-ide-border text-ide-text hover:border-ide-accent hover:text-ide-accent hover:shadow-md active:scale-95'}
                `}
                    >
                        {checking ? (
                            <RefreshCw className="w-4 h-4 animate-spin" />
                        ) : upToDate ? (
                            <CheckCircle2 className="w-4 h-4" />
                        ) : (
                            <RefreshCw className="w-4 h-4 group-hover:rotate-180 transition-transform duration-500" />
                        )}
                        {checking ? 'Checking...' : upToDate ? 'Lateset' : 'Check Now'}
                    </button>
                </div>

                {/* Donate Card - Special Style */}
                <div className="relative group overflow-hidden bg-gradient-to-br from-indigo-500/10 via-purple-500/5 to-ide-panel border border-indigo-500/20 rounded-2xl p-8 cursor-pointer transition-all duration-300 hover:-translate-y-1 hover:shadow-card">

                    {/* Background Decorations */}
                    {/* Overlapping Spheres */}
                    <div className="absolute -top-12 -left-12 w-40 h-40 rounded-full bg-gradient-to-br from-indigo-400/20 to-transparent blur-2xl group-hover:scale-110 transition-transform duration-700"></div>
                    <div className="absolute top-8 left-16 w-16 h-16 rounded-full bg-purple-400/10 blur-xl group-hover:translate-x-2 transition-transform duration-500"></div>

                    {/* Large Github Icon */}
                    <Github
                        className="absolute -bottom-8 -right-8 w-48 h-48 text-indigo-500/5 rotate-12 group-hover:scale-110 group-hover:rotate-[15deg] group-hover:text-indigo-500/10 transition-all duration-500 ease-out"
                        strokeWidth={1}
                    />

                    {/* Content */}
                    <div className="relative z-10">
                        <h2 className="text-3xl font-bold text-ide-text mb-3 group-hover:text-indigo-500 transition-colors">Check us on Github</h2>
                        <p className="text-ide-muted font-light leading-relaxed max-w-sm">
                            Star our repository and contribute to the development. Your support keeps the code flowing.
                        </p>
                    </div>
                </div>

            </div>
        </div>
    );
}