## Qwen Added Memories
- When specifying Rust dependency versions in Cargo.toml, always use major.minor precision (e.g., "1.5" instead of "1")
- For this project, do not fix dead_code clippy warnings - they are acceptable
- 此项目中 8000 端口用于真实服务。测试项目时需要先复制 config.toml 配置文件，修改为其他端口（如 9000）后再启动测试。
- 日志格式化时使用内联标识符，只有打印非标识符的内容时才在后方写入
- 日志内容使用英文
- 注释使用中文
