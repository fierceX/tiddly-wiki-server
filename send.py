import argparse
import json
import logging
import os
import sys
import time
from datetime import datetime
from pathlib import Path
from typing import Any, Dict, Optional, Tuple

import requests
from requests.exceptions import ConnectionError, RequestException, Timeout

# 设置日志
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(name)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

# 默认配置
DEFAULT_CONFIG = {
    "api_url": "http://127.0.0.1:3032/api/inbox",
    "api_token": "Basic ZmllcmNleDphZG1pbg==",
    "timeout": 30,
    "max_retries": 3,
    "retry_delay": 1,  # 初始重试延迟（秒）
    "user_agent": "HTTP-Backup-Skill/1.0"
}

class HTTPBackupClient:
    """HTTP备份客户端"""

    def __init__(self, config: Optional[Dict[str, Any]] = None):
        """
        初始化客户端

        Args:
            config: 配置字典，可包含：
                - api_url: API端点URL
                - api_token: API认证令牌
                - timeout: 请求超时时间（秒）
                - max_retries: 最大重试次数
                - retry_delay: 重试延迟（秒）
        """
        self.config = DEFAULT_CONFIG.copy()
        if config:
            self.config.update(config)

        # 从环境变量读取配置（优先级高于默认配置）
        self._load_from_env()

        # 从配置文件读取（优先级最高）
        self._load_from_config_file()

        # 验证必要配置
        if not self.config["api_url"]:
            logger.warning("API URL未配置，需要在使用时提供或通过环境变量设置")

    def _load_from_env(self):
        """从环境变量加载配置"""
        env_mapping = {
            "HTTP_BACKUP_API_URL": "api_url",
            "HTTP_BACKUP_API_TOKEN": "api_token",
            "HTTP_BACKUP_TIMEOUT": "timeout",
            "HTTP_BACKUP_MAX_RETRIES": "max_retries",
            "HTTP_BACKUP_RETRY_DELAY": "retry_delay"
        }

        for env_var, config_key in env_mapping.items():
            value = os.getenv(env_var)
            if value:
                # 类型转换
                if config_key in ["timeout", "max_retries", "retry_delay"]:
                    try:
                        self.config[config_key] = int(value)
                    except ValueError:
                        logger.warning(f"环境变量{env_var}的值'{value}'无法转换为整数，使用默认值")
                else:
                    self.config[config_key] = value

    def _load_from_config_file(self):
        """从配置文件加载配置"""
        config_paths = [
            Path.home() / ".http_backup_config.json",
            Path("/etc/http_backup_config.json"),
            Path("http_backup_config.json")
        ]

        for config_path in config_paths:
            if config_path.exists():
                try:
                    with open(config_path, 'r', encoding='utf-8') as f:
                        file_config = json.load(f)
                    self.config.update(file_config)
                    logger.info(f"从 {config_path} 加载配置")
                    break
                except (json.JSONDecodeError, IOError) as e:
                    logger.warning(f"读取配置文件 {config_path} 失败: {e}")

    def generate_payload(
        self,
        content: str,
        content_type: str,
        title: Optional[str] = None,
        tags: Optional[list] = None,
        metadata: Optional[Dict[str, Any]] = None
    ) -> Dict[str, Any]:
        """
        生成要发送的JSON数据

        Args:
            content: 内容文本
            content_type: 内容类型 (observation|conclusion|backup|summary)
            title: 自定义标题，如为None则自动生成
            tags: 标签列表
            metadata: 额外的元数据

        Returns:
            格式化后的数据字典
        """
        # 自动生成标题
        if not title:
            title_map = {
                "observation": "成长观察记录",
                "conclusion": "核心结论总结",
                "backup": "指定备份内容",
                "summary": "对话总结"
            }
            base_title = title_map.get(content_type, "备份内容")
            title = f"{base_title} - {datetime.now().strftime('%Y-%m-%d %H:%M')}"

        # 默认标签
        if not tags:
            tags = self._generate_default_tags(content_type)

        # 基础payload
        payload = {
            "type": content_type,
            "title": title,
            "tags": tags,
            "content": content,
            "timestamp": datetime.now().isoformat(),
            "context": self._generate_context_summary(),
            "metadata": {
                "source": "http-backup-skill",
                "version": "1.0",
                "priority": "medium"
            }
        }

        # 添加自定义元数据
        if metadata:
            payload["metadata"].update(metadata)

        return payload

    def _generate_default_tags(self, content_type: str) -> list:
        """生成默认标签"""
        tag_map = {
            "observation": ["成长记录", "观察", "发展评估"],
            "conclusion": ["核心结论", "教育策略", "总结"],
            "backup": ["备份", "重要信息", "存档"],
            "summary": ["总结", "对话记录", "里程碑"]
        }
        return tag_map.get(content_type, ["备份"])

    def _generate_context_summary(self) -> str:
        """生成上下文摘要"""
        # 这里可以添加更多上下文信息
        return f"通过HTTP备份技能保存，时间：{datetime.now().strftime('%Y-%m-%d %H:%M:%S')}"

    def send(
        self,
        content: str,
        content_type: str = "backup",
        title: Optional[str] = None,
        tags: Optional[list] = None,
        metadata: Optional[Dict[str, Any]] = None,
        api_url: Optional[str] = None,
        api_token: Optional[str] = None
    ) -> Tuple[bool, str]:
        """
        发送内容到API端点

        Args:
            content: 要发送的内容
            content_type: 内容类型
            title: 自定义标题
            tags: 标签列表
            metadata: 额外元数据
            api_url: 自定义API URL（覆盖配置）
            api_token: 自定义API Token（覆盖配置）

        Returns:
            (成功标志, 消息)
        """
        # 确定使用的API URL和Token
        url = api_url or self.config["api_url"]
        token = api_token or self.config["api_token"]

        if not url:
            return False, "错误：未配置API URL，请通过环境变量、配置文件或参数设置"

        # 生成payload
        payload = self.generate_payload(content, content_type, title, tags, metadata)

        # 准备请求头
        headers = {
            "User-Agent": self.config["user_agent"],
            "Content-Type": "application/json; charset=utf-8"
        }

        if token:
            headers["Authorization"] = f"{token}"

        # 重试逻辑
        max_retries = self.config["max_retries"]
        retry_delay = self.config["retry_delay"]

        for attempt in range(max_retries + 1):
            try:
                logger.info(f"发送请求到 {url} (尝试 {attempt + 1}/{max_retries + 1})")

                response = requests.post(
                    url,
                    json=payload,
                    headers=headers,
                    timeout=self.config["timeout"]
                )

                # 检查响应
                response.raise_for_status()

                # 尝试解析响应
                try:
                    result = response.json()
                    logger.info(f"请求成功: {response.status_code}")
                    return True, f"成功：{result.get('message', '数据已保存')}"
                except json.JSONDecodeError:
                    return True, f"成功：HTTP {response.status_code}"

            except Timeout:
                logger.warning(f"请求超时 (尝试 {attempt + 1})")
                if attempt < max_retries:
                    sleep_time = retry_delay * (2 ** attempt)  # 指数退避
                    logger.info(f"等待 {sleep_time} 秒后重试...")
                    time.sleep(sleep_time)
                else:
                    return False, "错误：请求超时，已达到最大重试次数"

            except ConnectionError as e:
                logger.warning(f"连接错误: {e} (尝试 {attempt + 1})")
                if attempt < max_retries:
                    sleep_time = retry_delay * (2 ** attempt)
                    logger.info(f"等待 {sleep_time} 秒后重试...")
                    time.sleep(sleep_time)
                else:
                    return False, f"错误：连接失败 - {str(e)}"

            except RequestException as e:
                error_msg = str(e)
                if hasattr(e, 'response') and e.response is not None:
                    status_code = e.response.status_code
                    error_msg = f"HTTP {status_code}: {error_msg}"

                    # 如果是认证错误，不重试
                    if status_code in [401, 403]:
                        return False, f"错误：认证失败 - {error_msg}"

                logger.warning(f"请求错误: {error_msg} (尝试 {attempt + 1})")
                if attempt < max_retries:
                    sleep_time = retry_delay * (2 ** attempt)
                    logger.info(f"等待 {sleep_time} 秒后重试...")
                    time.sleep(sleep_time)
                else:
                    return False, f"错误：请求失败 - {error_msg}"

            except Exception as e:
                logger.error(f"未知错误: {e}", exc_info=True)
                return False, f"错误：未知错误 - {str(e)}"

        return False, "错误：未知错误，请求失败"

def main():
    """命令行入口点"""
    parser = argparse.ArgumentParser(description="HTTP备份API客户端")

    # 内容相关参数
    content_group = parser.add_mutually_exclusive_group(required=True)
    content_group.add_argument("--content", help="要备份的内容文本")
    content_group.add_argument("--file", help="从文件读取内容")

    # 其他参数
    parser.add_argument("--type", default="backup",
                       choices=["observation", "conclusion", "backup", "summary"],
                       help="内容类型")
    parser.add_argument("--title", help="自定义标题")
    parser.add_argument("--tags", help="标签列表，用逗号分隔")
    parser.add_argument("--metadata", help="额外元数据，JSON格式字符串")

    # API配置参数（覆盖配置）
    parser.add_argument("--endpoint", help="API端点URL")
    parser.add_argument("--token", help="API认证令牌")

    # 输出选项
    parser.add_argument("--dry-run", action="store_true",
                       help="只生成payload不发送")
    parser.add_argument("--debug", action="store_true",
                       help="启用调试输出")

    args = parser.parse_args()

    # 设置日志级别
    if args.debug:
        logging.getLogger().setLevel(logging.DEBUG)
        logger.debug("调试模式已启用")

    # 读取内容
    if args.file:
        try:
            with open(args.file, 'r', encoding='utf-8') as f:
                content = f.read()
        except IOError as e:
            logger.error(f"读取文件失败: {e}")
            sys.exit(1)
    else:
        content = args.content

    # 解析标签
    tags = None
    if args.tags:
        tags = [tag.strip() for tag in args.tags.split(",")]

    # 解析元数据
    metadata = None
    if args.metadata:
        try:
            metadata = json.loads(args.metadata)
        except json.JSONDecodeError as e:
            logger.error(f"解析元数据失败: {e}")
            sys.exit(1)

    # 创建客户端
    client = HTTPBackupClient()

    # 生成payload
    payload = client.generate_payload(
        content=content,
        content_type=args.type,
        title=args.title,
        tags=tags,
        metadata=metadata
    )

    # 如果是dry-run，只显示payload
    if args.dry_run:
        print("=== 生成的Payload ===")
        print(json.dumps(payload, ensure_ascii=False, indent=2))
        print("\n=== 配置信息 ===")
        print(f"API URL: {client.config.get('api_url', '未配置')}")
        print(f"超时时间: {client.config.get('timeout')}秒")
        print(f"最大重试次数: {client.config.get('max_retries')}")
        sys.exit(0)

    # 发送请求
    success, message = client.send(
        content=content,
        content_type=args.type,
        title=args.title,
        tags=tags,
        metadata=metadata,
        api_url=args.endpoint,
        api_token=args.token
    )

    # 输出结果
    if success:
        print(f"✅ {message}")
        sys.exit(0)
    else:
        print(f"❌ {message}")
        sys.exit(1)

if __name__ == "__main__":
    main()