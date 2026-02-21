<div align="center">

# CarbonPaper - 复写纸
![carbonpaper-screenshots](./docs/imgs/carbonpaper-screenshots.png)

<p align="center">
  |
  <a href="./README.en.md">English</a> |
  <strong>中文</strong> |
</p>

[![Release](https://github.com/White-NX/carbonPaper/actions/workflows/release.yml/badge.svg)](https://github.com/White-NX/carbonPaper/actions/workflows/release.yml)

</div>

## Description

CarbonPaper 是一个利用开源OCR和向量存储解决方案的，旨在帮助查找在电脑上看到的任何内容的开源程序。

## Download and Install

可以直接使用 [Releases](https://github.com/White-NX/carbonPaper/releases) 中的预构建版本，程序会自动下载和安装其所需依赖

## Why Carbonpaper?

`CarbonPaper`，中文译名复写纸。复写纸是一种特殊的纸张，它的使用方式是在它下面放置一张另外一张纸（通常薄一些），用力的在上层纸张上写字，颜色将会因压力而转到下层纸上，从而起到复写的作用。

CarbonPaper（本程序） 具有以下功能：

- 记录用户屏幕所见。
- 借由 Windows CNG，利用用户系统自带的安全硬/软件加密存储用户快照。
- 除经用户允许，任何数据将不会离开用户的计算机。
- 支持 OCR 关键词搜索所见文本，同时也支持使用自然语言的图片特征搜索快照。
- 经由时间线快速预览快照。
- 不需要 Copilot+ 验证，相关服务全部通过 CPU 而非 NPU 以低占用的方式进行。

## Requirements

OS: Windows 10 1809 or later (of course)

Architecture: x64

Internet Access: Yes **（init）**

## API
未来的 CarbonPaper 会提供开放API的功能，以允许用户使用 AI 进行快照的增删改查

## Main open source libs used

- 文本OCR：[RapidOCR-onnxruntime](https://github.com/RapidAI/RapidOCR)
- 向量数据库：[ChromaDB](https://github.com/chroma-core/chroma)
