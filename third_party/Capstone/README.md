# Capstone 5.0.6

用于本地 x86/x64 PE 代码分析，宿主为 Windows x64。直接调用原生库，不安装 Python 或 pip。

- 上游：https://github.com/capstone-engine/capstone
- 分发包：https://pypi.org/project/capstone/5.0.6/#files
- 文件：`capstone-5.0.6-py3-none-win_amd64.whl` 中的 `capstone/lib/capstone.dll`
- Wheel SHA-256：`761c3deae00b22ac697081cdae1383bb90659dd0d79387a09cf5bdbb22b17064`
- DLL SHA-256：`651096794b3c3c2a1df24345e22844a0311fca56a4f74ea1cfa29107983760e1`
- 许可：[BSD 3-Clause](LICENSE.TXT)

发行包携带 DLL 和许可证。二进制未修改；加载时校验 DLL 哈希及 Capstone API 版本。
