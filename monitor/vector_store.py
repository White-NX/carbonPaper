"""
向量存储模块 - Chinese-CLIP图片向量化 + ChromaDB存储
使用 OFA-Sys/chinese-clip-vit-base-patch16 模型进行图片和文本的向量化
"""
import os
import hashlib
import logging
import time as _time
from typing import List, Dict, Any, Optional, Union
from PIL import Image
import numpy as np

logger = logging.getLogger(__name__)

# 延迟导入，避免启动时加载过慢
_clip_instance = None


class ChineseCLIPSingleton:
    """Chinese-CLIP 模型单例封装"""
    
    _instance = None
    _model = None
    _processor = None
    _initialized = False
    
    def __new__(cls):
        if cls._instance is None:
            cls._instance = super(ChineseCLIPSingleton, cls).__new__(cls)
        return cls._instance
        
    def initialize(self):
        """初始化加载模型"""
        if self._initialized:
            return
            
        logger.info("正在加载 Chinese-CLIP 模型 (常驻内存)...")

        model_name = os.environ.get('MODEL_PATH', None)
        if not model_name:
            model_name = os.path.abspath(os.path.join(os.environ.get('LOCALAPPDATA', os.path.expanduser('~')), "carbonPaper", "models"))

        from transformers import ChineseCLIPModel, ChineseCLIPProcessor
        
        # model_name = "OFA-Sys/chinese-clip-vit-base-patch16"
        try:
            # [Fix] Use low_cpu_mem_usage=False to disable lazy loading (meta tensor)
            # This avoids "Cannot copy out of meta tensor" error in newer transformers versions
            self._processor = ChineseCLIPProcessor.from_pretrained(
                model_name,
                use_fast=False,  # Use slow processor to avoid torchvision dependency
            )
            self._model = ChineseCLIPModel.from_pretrained(
                model_name,
                low_cpu_mem_usage=False,  # Disable meta tensor lazy loading
            )
            self._model.eval()
            self._initialized = True
            logger.info("Chinese-CLIP 模型加载完成")
        except Exception as e:
            logger.error("Chinese-CLIP 模型加载失败: %s", e)
            raise
            
    def get_components(self):
        """获取模型组件"""
        if not self._initialized:
            self.initialize()
        return self._model, self._processor


def _load_clip_model():
    """获取单例模型实例"""
    singleton = ChineseCLIPSingleton()
    return singleton.get_components()


class ImageVectorizer:
    """图片向量化器 - 使用Chinese-CLIP"""
    
    def __init__(self):
        """初始化向量化器"""
        self.model = None
        self.processor = None
        self._singleton = ChineseCLIPSingleton()
    
    def _ensure_initialized(self):
        """确保模型已初始化"""
        if self.model is None:
            self.model, self.processor = self._singleton.get_components()
    
    def encode_image(self, image: Union[str, Image.Image, np.ndarray]) -> np.ndarray:
        """
        将图片编码为向量
        
        Args:
            image: 图片路径、PIL Image对象或numpy数组
            
        Returns:
            图片特征向量 (归一化后)
        """
        self._ensure_initialized()
        import torch

        # 处理不同类型的输入
        if isinstance(image, str):
            image = Image.open(image).convert('RGB')
        elif isinstance(image, np.ndarray):
            image = Image.fromarray(image).convert('RGB')
        elif isinstance(image, Image.Image):
            image = image.convert('RGB')

        # 处理图片
        inputs = self.processor(images=image, return_tensors="pt")
        # Ensure inputs are on the same device as model
        inputs = {k: v.to(self.model.device) for k, v in inputs.items() if v is not None}

        with torch.no_grad():
            image_features = self.model.get_image_features(**inputs)

            # Some transformers versions may return a BaseModelOutput... instead of a tensor
            if not isinstance(image_features, torch.Tensor):
                # Try to extract pooled output
                try:
                    if hasattr(image_features, 'pooler_output') and image_features.pooler_output is not None:
                        pooled = image_features.pooler_output
                    else:
                        pooled = image_features.last_hidden_state[:, 0, :]
                except Exception:
                    # Fallback: call vision_model directly
                    try:
                        vm_out = self.model.vision_model(**inputs)
                        if hasattr(vm_out, 'pooler_output') and vm_out.pooler_output is not None:
                            pooled = vm_out.pooler_output
                        else:
                            pooled = vm_out.last_hidden_state[:, 0, :]
                    except Exception:
                        raise RuntimeError('Unable to extract image features from model output')

                # Apply projection layer if available
                proj = None
                for name in ('vision_projection', 'visual_projection', 'image_projection', 'visual_head', 'vision_proj', 'image_proj'):
                    proj = getattr(self.model, name, None)
                    if proj is not None:
                        break

                if proj is not None:
                    import torch as _torch
                    # If callable module (e.g., nn.Linear)
                    if hasattr(proj, '__call__') and not isinstance(proj, _torch.Tensor):
                        # Try to detect expected input dim
                        expected_in = None
                        try:
                            if hasattr(proj, 'weight') and isinstance(getattr(proj, 'weight'), _torch.Tensor):
                                expected_in = int(getattr(proj, 'weight').shape[1])
                            elif hasattr(proj, 'in_features'):
                                expected_in = int(getattr(proj, 'in_features'))
                        except Exception:
                            expected_in = None

                        Dp = pooled.shape[-1]
                        if expected_in is not None and expected_in != Dp:
                            # Search for alternative linear-like projection in model that matches pooled dim
                            found = None
                            found_name = None
                            for attr in dir(self.model):
                                try:
                                    cand = getattr(self.model, attr)
                                    if hasattr(cand, 'weight') and isinstance(getattr(cand, 'weight'), _torch.Tensor):
                                        if int(cand.weight.shape[1]) == Dp:
                                            found = cand
                                            found_name = attr
                                            break
                                except Exception:
                                    continue

                            if found is not None:
                                logger.info("[vector_store] using alternative projection '%s' for pooled dim %s", found_name, Dp)
                                try:
                                    image_features = found(pooled)
                                except Exception as e:
                                    logger.error("[vector_store] alternative projection call failed: %s", e)
                                    raise
                            else:
                                # logger.warning("[vector_store] projection expected_in=%s != pooled_dim=%s; no compatible projection found, skipping projection", expected_in, Dp)
                                image_features = pooled
                        else:
                            try:
                                image_features = proj(pooled)
                            except Exception as e:
                                logger.error("[vector_store] projection module call failed: %s", e)
                                raise
                    else:
                        # proj is likely a Parameter/Tensor - handle multiplication with correct orientation
                        try:
                            w = proj
                            if not isinstance(w, _torch.Tensor):
                                w = _torch.tensor(w, device=pooled.device)
                            Dp = pooled.shape[-1]
                            if w.ndim == 2:
                                # If w is (out, in)
                                if w.shape[1] == Dp:
                                    image_features = pooled @ w.t()
                                # If w is (in, out)
                                elif w.shape[0] == Dp:
                                    image_features = pooled @ w
                                else:
                                    raise RuntimeError(f"Projection tensor shape {tuple(w.shape)} incompatible with pooled dim {Dp}")
                            else:
                                raise RuntimeError(f"Unsupported projection tensor ndim={w.ndim}")
                        except Exception as e:
                            logger.error("[vector_store] projection tensor multiply failed: %s", e)
                            raise
                else:
                    image_features = pooled

            # 归一化
            image_features = image_features / image_features.norm(dim=-1, keepdim=True)

        return image_features.cpu().numpy().flatten()
    
    def encode_text(self, text: str) -> np.ndarray:
        """
        将文本编码为向量

        Args:
            text: 要编码的文本

        Returns:
            文本特征向量 (归一化后)
        """
        _t_total = _time.perf_counter()
        self._ensure_initialized()
        import torch

        _t0 = _time.perf_counter()
        inputs = self.processor(text=[text], return_tensors="pt", padding=True)

        # Filter out None values and move tensors to model device
        inputs = {k: v.to(self.model.device) for k, v in inputs.items() if v is not None}
        _t_tokenize = _time.perf_counter() - _t0

        _t0 = _time.perf_counter()
        with torch.no_grad():
            text_outputs = self.model.text_model(**inputs)

            if hasattr(text_outputs, 'pooler_output') and text_outputs.pooler_output is not None:
                pooled_output = text_outputs.pooler_output
            else:
                pooled_output = text_outputs.last_hidden_state[:, 0, :]

            text_features = self.model.text_projection(pooled_output)
            # normalize
            text_features = text_features / text_features.norm(dim=-1, keepdim=True)
        _t_inference = _time.perf_counter() - _t0

        arr = text_features.cpu().numpy().flatten()

        _elapsed = _time.perf_counter() - _t_total
        _log_fn = logger.warning if _elapsed > 10 else logger.info
        _log_fn(
            "[DIAG:encode_text] tokenize=%.3fs inference=%.3fs total=%.3fs device=%s",
            _t_tokenize, _t_inference, _elapsed,
            self.model.device
        )
        return arr
    
    def encode_images_batch(self, images: List[Union[str, Image.Image]]) -> np.ndarray:
        """批量编码图片"""
        self._ensure_initialized()
        import torch
        
        pil_images = []
        for img in images:
            if isinstance(img, str):
                pil_images.append(Image.open(img).convert('RGB'))
            elif isinstance(img, np.ndarray):
                pil_images.append(Image.fromarray(img).convert('RGB'))
            else:
                pil_images.append(img.convert('RGB'))
        
        inputs = self.processor(images=pil_images, return_tensors="pt", padding=True)
        # Ensure inputs are on the same device as model
        inputs = {k: v.to(self.model.device) for k, v in inputs.items() if v is not None}

        import torch
        with torch.no_grad():
            image_features = self.model.get_image_features(**inputs)

            if not isinstance(image_features, torch.Tensor):
                try:
                    if hasattr(image_features, 'pooler_output') and image_features.pooler_output is not None:
                        pooled = image_features.pooler_output
                    else:
                        pooled = image_features.last_hidden_state[:, 0, :]
                except Exception:
                    vm_out = self.model.vision_model(**inputs)
                    if hasattr(vm_out, 'pooler_output') and vm_out.pooler_output is not None:
                        pooled = vm_out.pooler_output
                    else:
                        pooled = vm_out.last_hidden_state[:, 0, :]

                proj = None
                for name in ('vision_projection', 'visual_projection', 'image_projection', 'visual_head', 'vision_proj', 'image_proj'):
                    proj = getattr(self.model, name, None)
                    if proj is not None:
                        break

                if proj is not None:
                    import torch as _torch
                    if hasattr(proj, '__call__') and not isinstance(proj, _torch.Tensor):
                        expected_in = None
                        try:
                            if hasattr(proj, 'weight') and isinstance(getattr(proj, 'weight'), _torch.Tensor):
                                expected_in = int(getattr(proj, 'weight').shape[1])
                            elif hasattr(proj, 'in_features'):
                                expected_in = int(getattr(proj, 'in_features'))
                        except Exception:
                            expected_in = None

                        Dp = pooled.shape[-1]
                        if expected_in is not None and expected_in != Dp:
                            found = None
                            found_name = None
                            for attr in dir(self.model):
                                try:
                                    cand = getattr(self.model, attr)
                                    if hasattr(cand, 'weight') and isinstance(getattr(cand, 'weight'), _torch.Tensor):
                                        if int(cand.weight.shape[1]) == Dp:
                                            found = cand
                                            found_name = attr
                                            break
                                except Exception:
                                    continue

                            if found is not None:
                                logger.info("[vector_store] using alternative projection '%s' for pooled dim %s", found_name, Dp)
                                image_features = found(pooled)
                            else:
                                # logger.warning("[vector_store] projection expected_in=%s != pooled_dim=%s; no compatible projection found, skipping projection", expected_in, Dp)
                                image_features = pooled
                        else:
                            try:
                                image_features = proj(pooled)
                            except Exception as e:
                                logger.error("[vector_store] projection module call failed: %s", e)
                                raise
                    else:
                        try:
                            w = proj
                            if not isinstance(w, _torch.Tensor):
                                w = _torch.tensor(w, device=pooled.device)
                            Dp = pooled.shape[-1]
                            if w.ndim == 2:
                                if w.shape[1] == Dp:
                                    image_features = pooled @ w.t()
                                elif w.shape[0] == Dp:
                                    image_features = pooled @ w
                                else:
                                    raise RuntimeError(f"Projection tensor shape {tuple(w.shape)} incompatible with pooled dim {Dp}")
                            else:
                                raise RuntimeError(f"Unsupported projection tensor ndim={w.ndim}")
                        except Exception as e:
                            logger.error("[vector_store] projection tensor multiply failed: %s", e)
                            raise
                else:
                    image_features = pooled

            image_features = image_features / image_features.norm(dim=-1, keepdim=True)

        return image_features.cpu().numpy()
    
    def compute_similarity(
        self, 
        image: Union[str, Image.Image], 
        text: str
    ) -> float:
        """
        计算图片和文本的相似度
        
        Args:
            image: 图片
            text: 文本
            
        Returns:
            相似度分数 (0-1)
        """
        image_vec = self.encode_image(image)
        text_vec = self.encode_text(text)
        
        # 余弦相似度 (已归一化，直接点积)
        similarity = np.dot(image_vec, text_vec)
        return float(similarity)


class VectorStore:
    """向量存储 - 基于ChromaDB"""
    
    def __init__(
        self, 
        collection_name: str = "screenshot_embeddings",
        persist_directory: str = "./chroma_db",
        storage_client = None
    ):
        """
        初始化向量存储
        
        Args:
            collection_name: ChromaDB集合名称
            persist_directory: 持久化目录
            storage_client: 存储客户端（用于加密明文数据）
        """
        import chromadb
        from chromadb.config import Settings
        
        self.persist_directory = persist_directory
        self.collection_name = collection_name
        self.storage_client = storage_client  # 用于加密明文数据
        
        # 初始化ChromaDB客户端（持久化模式）
        self.client = chromadb.PersistentClient(
            path=persist_directory,
            settings=Settings(anonymized_telemetry=False)
        )
        
        # 获取或创建集合
        self.collection = self.client.get_or_create_collection(
            name=collection_name,
            metadata={"hnsw:space": "cosine"}  # 使用余弦相似度
        )
        
        # 初始化向量化器
        self.vectorizer = ImageVectorizer()
        # Diagnostic: print collection basic info
        try:
            count = self.collection.count()
        except Exception:
            count = None
        try:
            abs_path = os.path.abspath(self.persist_directory)
            path_exists = os.path.exists(abs_path)
            path_list = os.listdir(abs_path) if path_exists else []
        except Exception:
            abs_path = self.persist_directory
            path_exists = False
            path_list = []
        logger.info("[vector_store] Initialized VectorStore collection='%s' persist='%s' exists=%s count=%s files=%s encrypted=%s", self.collection_name, abs_path, path_exists, count, path_list, storage_client is not None)
    
    def _encrypt_text(self, text: str) -> str:
        """加密文本（如果有存储客户端）"""
        if self.storage_client and text:
            encrypted = self.storage_client.encrypt_for_chromadb(text)
            if encrypted:
                return encrypted
        return text
    
    def _decrypt_text(self, text: str) -> str:
        """解密文本（如果有存储客户端）"""
        if self.storage_client and text:
            # 检查是否是加密数据（以 ENC2:/ENC: 前缀标识）
            if text.startswith("ENC2:") or text.startswith("ENC:"):
                decrypted = self.storage_client.decrypt_from_chromadb(text)
                if decrypted is not None:
                    return decrypted
        return text

    def _decrypt_texts(self, texts: List[str]) -> List[str]:
        """批量解密文本（保持输入顺序）"""
        if not self.storage_client or not texts:
            return texts

        encrypted_values = []
        encrypted_indices = []

        for idx, text in enumerate(texts):
            if isinstance(text, str) and (text.startswith("ENC2:") or text.startswith("ENC:")):
                encrypted_indices.append(idx)
                encrypted_values.append(text)

        if not encrypted_values:
            return texts

        decrypt_many = getattr(self.storage_client, 'decrypt_many_from_chromadb', None)
        if callable(decrypt_many):
            decrypted_list = decrypt_many(encrypted_values)
        else:
            decrypted_list = [self.storage_client.decrypt_from_chromadb(v) for v in encrypted_values]

        result = list(texts)
        for i, idx in enumerate(encrypted_indices):
            decrypted = decrypted_list[i] if i < len(decrypted_list) else None
            if decrypted is not None:
                result[idx] = decrypted

        return result
    
    def _decrypt_metadata(self, meta: Dict[str, Any]) -> Dict[str, Any]:
        """解密元数据中的加密字段"""
        if not meta or not self.storage_client:
            return meta
        
        decrypted = dict(meta)
        # 需要解密的字段
        encrypted_fields = {'image_path', 'window_title', 'process_name', 'app_name', 'url'}

        batch_values = []
        batch_keys = []
        for k in encrypted_fields:
            v = meta.get(k)
            if isinstance(v, str) and (v.startswith("ENC2:") or v.startswith("ENC:")):
                batch_keys.append(k)
                batch_values.append(v)

        if batch_values:
            decrypted_values = self._decrypt_texts(batch_values)
            for i, key in enumerate(batch_keys):
                value = decrypted_values[i] if i < len(decrypted_values) else None
                if value is not None:
                    decrypted[key] = value
        
        return decrypted
    
    def _decrypt_result(self, result: Dict[str, Any]) -> Dict[str, Any]:
        """解密单个搜索结果"""
        if not result:
            return result

        decrypted = result.copy()

        batch_targets = []
        batch_keys = []

        if isinstance(decrypted.get('image_path'), str):
            batch_keys.append('image_path')
            batch_targets.append(decrypted['image_path'])

        if isinstance(decrypted.get('ocr_text'), str):
            batch_keys.append('ocr_text')
            batch_targets.append(decrypted['ocr_text'])

        if batch_targets:
            decrypted_values = self._decrypt_texts(batch_targets)
            for i, key in enumerate(batch_keys):
                value = decrypted_values[i] if i < len(decrypted_values) else None
                if value is not None:
                    decrypted[key] = value

        # decrypt metadata
        if isinstance(decrypted.get('metadata'), dict):
            decrypted['metadata'] = self._decrypt_metadata(decrypted['metadata'])

        return decrypted

    def _decrypt_results_batch(self, results: List[Dict]) -> List[Dict]:
        """批量解密多条搜索结果（单次 IPC 调用）"""
        if not results or not self.storage_client:
            return results

        # Collect all encrypted values from top-level and metadata fields
        encrypted_to_index: Dict[str, int] = {}
        all_unique: List[str] = []

        # top-leve
        top_level_fields = ('image_path', 'ocr_text')
        # fields in metadata that may contain encrypted values
        meta_fields = ('image_path', 'window_title', 'process_name', 'app_name', 'url')

        def _collect(value: Any):
            if isinstance(value, str) and (value.startswith("ENC2:") or value.startswith("ENC:")):
                if value not in encrypted_to_index:
                    encrypted_to_index[value] = len(all_unique)
                    all_unique.append(value)

        for r in results:
            for key in top_level_fields:
                _collect(r.get(key))
            meta = r.get('metadata')
            if isinstance(meta, dict):
                for key in meta_fields:
                    _collect(meta.get(key))

        if not all_unique:
            return results

        # single batch decryption
        decrypted_list = self._decrypt_texts(all_unique)
        decrypt_map: Dict[str, str] = {}
        for i, enc_val in enumerate(all_unique):
            dec_val = decrypted_list[i] if i < len(decrypted_list) else None
            if dec_val is not None:
                decrypt_map[enc_val] = dec_val

        def _resolve(value: Any) -> Any:
            if isinstance(value, str) and value in decrypt_map:
                return decrypt_map[value]
            return value

        # backfill decrypted values into results
        out = []
        for r in results:
            d = r.copy()
            for key in top_level_fields:
                if key in d:
                    d[key] = _resolve(d[key])
            meta = d.get('metadata')
            if isinstance(meta, dict):
                new_meta = dict(meta)
                for key in meta_fields:
                    if key in new_meta:
                        new_meta[key] = _resolve(new_meta[key])
                d['metadata'] = new_meta
            out.append(d)

        return out
    
    @staticmethod
    def _compute_id(image_path: str) -> str:
        """根据图片路径生成唯一ID"""
        return hashlib.md5(image_path.encode()).hexdigest()
    
    def add_image(
        self,
        image_path: str,
        image: Optional[Union[Image.Image, np.ndarray]] = None,
        metadata: Optional[Dict[str, Any]] = None,
        ocr_text: Optional[str] = None
    ) -> str:
        """
        添加图片到向量存储
        
        Args:
            image_path: 图片路径（用于生成ID和存储引用）
            image: 图片对象（可选，不提供则从路径加载）
            metadata: 元数据
            ocr_text: OCR识别的文本（作为元数据存储）
            
        Returns:
            存储的ID
        """
        doc_id = self._compute_id(image_path)

        logger.info("[vector_store] add_image called image_path=%s doc_id=%s", image_path, doc_id)

        # 检查是否已存在
        try:
            existing = self.collection.get(ids=[doc_id])
            # Diagnostic: show what get returned
            try:
                existing_ids = existing.get('ids') if isinstance(existing, dict) else None
            except Exception:
                existing_ids = None
            logger.info("[vector_store] collection.get(ids=[...]) returned type=%s ids_preview=%s", type(existing), existing_ids)
        except Exception as e:
            # Some Chroma versions may not accept ids in get; fallback to None
            existing = None
            logger.info("[vector_store] collection.get(ids=[doc_id]) raised: %s", e)

        if existing and existing.get('ids'):
            logger.info("[vector_store] add_image skipped, already exists: %s", doc_id)
            return doc_id

        # 编码图片
        if image is None:
            image = image_path

        embedding = self.vectorizer.encode_image(image)
        logger.info("[vector_store] add_image -> embedding len=%d", len(embedding))
        
        # 准备元数据
        meta = {
            'image_path': self._encrypt_text(image_path),  # 加密图片路径
            'added_at': str(np.datetime64('now'))
        }
        if metadata:
            # ChromaDB元数据只支持字符串、整数、浮点数
            # 需要加密的敏感字段
            sensitive_fields = {'window_title', 'process_name', 'app_name', 'url'}
            for k, v in metadata.items():
                if isinstance(v, (str, int, float, bool)):
                    # 对敏感字段进行加密
                    if k in sensitive_fields and isinstance(v, str):
                        meta[k] = self._encrypt_text(v)
                    else:
                        meta[k] = v
                else:
                    str_val = str(v)
                    if k in sensitive_fields:
                        meta[k] = self._encrypt_text(str_val)
                    else:
                        meta[k] = str_val
        
        # 准备文档（OCR文本作为可搜索的文档内容）- 加密
        document = self._encrypt_text(ocr_text) if ocr_text else ""
        
        # 添加到集合
        try:
            # Diagnostic counts before add
            try:
                before = self.collection.count()
            except Exception:
                before = None
            logger.info("[vector_store] before add count=%s", before)
            logger.info("[vector_store] attempting add id=%s embeddings_len=%d document_len=%d", doc_id, len(embedding), len(document) if document else 0)

            self.collection.add(
                ids=[doc_id],
                embeddings=[embedding.tolist()],
                metadatas=[meta],
                documents=[document]
            )

            # Attempt to persist if client supports it
            try:
                persist_fn = getattr(self.client, 'persist', None)
                if callable(persist_fn):
                    persist_fn()
                    logger.info("[vector_store] client.persist() called")
            except Exception as e:
                logger.error("[vector_store] client.persist() call failed: %s", e)

            try:
                after = self.collection.count()
            except Exception:
                after = None
            logger.info("[vector_store] add_image success id=%s before=%s after=%s", doc_id, before, after)
        except Exception as e:
            logger.error("[vector_store] add_image failed id=%s error=%s", doc_id, e)

        return doc_id
    
    def add_images_batch(
        self,
        image_data: List[Dict[str, Any]]
    ) -> List[str]:
        """
        批量添加图片
        
        Args:
            image_data: 图片数据列表，每项包含:
                - image_path: 图片路径
                - image: 图片对象（可选）
                - metadata: 元数据（可选）
                - ocr_text: OCR文本（可选）
                
        Returns:
            添加的ID列表
        """
        ids = []
        embeddings = []
        metadatas = []
        documents = []
        
        for data in image_data:
            image_path = data['image_path']
            doc_id = self._compute_id(image_path)
            logger.info("[vector_store] add_images_batch processing %s -> %s", image_path, doc_id)
            # 检查是否已存在
            try:
                existing = self.collection.get(ids=[doc_id])
            except Exception:
                existing = None
            if existing and existing.get('ids'):
                logger.info("[vector_store] add_images_batch skipped existing %s", doc_id)
                ids.append(doc_id)
                continue
            
            # 编码图片
            image = data.get('image', image_path)
            embedding = self.vectorizer.encode_image(image)
            
            # 准备元数据 - 加密敏感字段
            meta = {
                'image_path': self._encrypt_text(image_path),
                'added_at': str(np.datetime64('now'))
            }
            if 'metadata' in data and data['metadata']:
                sensitive_fields = {'window_title', 'process_name', 'app_name', 'url'}
                for k, v in data['metadata'].items():
                    if isinstance(v, (str, int, float, bool)):
                        if k in sensitive_fields and isinstance(v, str):
                            meta[k] = self._encrypt_text(v)
                        else:
                            meta[k] = v
                    else:
                        str_val = str(v)
                        if k in sensitive_fields:
                            meta[k] = self._encrypt_text(str_val)
                        else:
                            meta[k] = str_val
            
            ids.append(doc_id)
            embeddings.append(embedding.tolist())
            metadatas.append(meta)
            # 加密 OCR 文本
            ocr_text = data.get('ocr_text', '')
            documents.append(self._encrypt_text(ocr_text) if ocr_text else '')
        
        if embeddings:
            try:
                self.collection.add(
                    ids=ids,
                    embeddings=embeddings,
                    metadatas=metadatas,
                    documents=documents
                )
                try:
                    before = self.collection.count()
                except Exception:
                    before = None
                try:
                    persist_fn = getattr(self.client, 'persist', None)
                    if callable(persist_fn):
                        persist_fn()
                        logger.info("[vector_store] client.persist() called (batch)")
                except Exception as e:
                    logger.error("[vector_store] client.persist() call failed (batch): %s", e)
                try:
                    after = self.collection.count()
                except Exception:
                    after = None
                try:
                    abs_path = os.path.abspath(self.persist_directory)
                    path_exists = os.path.exists(abs_path)
                    path_list = os.listdir(abs_path) if path_exists else []
                except Exception:
                    abs_path = self.persist_directory
                    path_exists = False
                    path_list = []
                logger.info("[vector_store] add_images_batch added %d items before=%s after=%s persist_path=%s exists=%s files=%s", len(ids), before, after, abs_path, path_exists, path_list)
            except Exception as e:
                logger.error("[vector_store] add_images_batch failed: %s", e)
            

        return ids
    
    def search_by_text(
        self,
        query: str,
        n_results: int = 10,
        min_similarity: float = 0.32
    ) -> List[Dict[str, Any]]:
        """
        使用自然语言搜索图片

        Args:
            query: 搜索查询文本
            n_results: 返回结果数量
            min_similarity: 最小相似度阈值 (0-1)

        Returns:
            搜索结果列表
        """
        _t_total = _time.perf_counter()

        # 将查询文本编码为向量
        _t0 = _time.perf_counter()
        query_embedding = self.vectorizer.encode_text(query)
        _t_encode = _time.perf_counter() - _t0

        # 在ChromaDB中搜索
        _t0 = _time.perf_counter()
        try:
            results = self.collection.query(
                query_embeddings=[query_embedding.tolist()],
                n_results=n_results,
                include=['metadatas', 'documents', 'distances']
            )
        except Exception as e:
            logger.error("[vector_store] collection.query failed: %s", e)
            raise
        _t_chromadb = _time.perf_counter() - _t0

        # 格式化结果
        try:
            ids_list = results['ids'][0] if results and results['ids'] else []
            distances_list = results['distances'][0] if results and results['distances'] else []
            docs_list = results['documents'][0] if results and results['documents'] else []
        except Exception:
            ids_list = []
            distances_list = []
            docs_list = []

        formatted_results = []
        for i, doc_id in enumerate(ids_list):
            distance = distances_list[i] if i < len(distances_list) else 1.0
            similarity = 1 - distance

            try:
                meta = results['metadatas'][0][i]
                ocr = docs_list[i] if i < len(docs_list) else None
            except Exception:
                meta = {}
                ocr = None

            # 过滤低置信度结果
            if similarity < min_similarity:
                continue

            formatted_results.append({
                'id': doc_id,
                'image_path': meta.get('image_path') if isinstance(meta, dict) else None,
                'metadata': meta,
                'ocr_text': ocr,
                'distance': distance,
                'similarity': similarity
            })

        # 解密结果中的敏感数据
        _t0 = _time.perf_counter()
        decrypted = self._decrypt_results_batch(formatted_results)
        _t_decrypt = _time.perf_counter() - _t0

        _elapsed_total = _time.perf_counter() - _t_total
        _log_fn = logger.warning if _elapsed_total > 10 else logger.info
        _log_fn(
            "[DIAG:search_by_text] encode=%.3fs chromadb=%.3fs decrypt=%.3fs "
            "candidates=%d filtered=%d total=%.3fs",
            _t_encode, _t_chromadb, _t_decrypt,
            len(ids_list), len(formatted_results),
            _elapsed_total
        )
        return decrypted

    def search_by_image(
        self,
        image: Union[str, Image.Image, np.ndarray],
        n_results: int = 10
    ) -> List[Dict[str, Any]]:
        """
        使用图片搜索相似图片
        
        Args:
            image: 查询图片
            n_results: 返回结果数量
            
        Returns:
            搜索结果列表
        """
        # 编码查询图片
        query_embedding = self.vectorizer.encode_image(image)
        
        # 搜索
        results = self.collection.query(
            query_embeddings=[query_embedding.tolist()],
            n_results=n_results,
            include=['metadatas', 'documents', 'distances']
        )
        
        # 格式化结果
        formatted_results = []
        if results and results['ids']:
            for i, doc_id in enumerate(results['ids'][0]):
                formatted_results.append({
                    'id': doc_id,
                    'image_path': results['metadatas'][0][i].get('image_path'),
                    'metadata': results['metadatas'][0][i],
                    'ocr_text': results['documents'][0][i] if results['documents'] else None,
                    'distance': results['distances'][0][i] if results['distances'] else None,
                    'similarity': 1 - results['distances'][0][i] if results['distances'] else None
                })
        
        # 解密结果中的敏感数据
        return self._decrypt_results_batch(formatted_results)

    def search_by_ocr_text(
        self,
        query: str,
        n_results: int = 10
    ) -> List[Dict[str, Any]]:
        """
        搜索OCR文本内容（全文搜索）
        
        Args:
            query: 搜索文本
            n_results: 返回结果数量
            
        Returns:
            搜索结果列表
        """
        # ChromaDB的文档搜索
        results = self.collection.query(
            query_texts=[query],
            n_results=n_results,
            include=['metadatas', 'documents', 'distances']
        )
        
        formatted_results = []
        if results and results['ids']:
            for i, doc_id in enumerate(results['ids'][0]):
                formatted_results.append({
                    'id': doc_id,
                    'image_path': results['metadatas'][0][i].get('image_path'),
                    'metadata': results['metadatas'][0][i],
                    'ocr_text': results['documents'][0][i] if results['documents'] else None,
                    'distance': results['distances'][0][i] if results['distances'] else None
                })
        
        # 解密结果中的敏感数据
        return self._decrypt_results_batch(formatted_results)

    def delete_image(self, image_path: str) -> bool:
        """删除图片"""
        doc_id = self._compute_id(image_path)
        try:
            self.collection.delete(ids=[doc_id])
            return True
        except Exception:
            return False
    
    def get_collection_stats(self) -> Dict[str, Any]:
        """获取集合统计信息"""
        return {
            'name': self.collection_name,
            'count': self.collection.count(),
            'persist_directory': self.persist_directory
        }
    
    def clear_collection(self):
        """清空集合"""
        self.client.delete_collection(self.collection_name)
        self.collection = self.client.get_or_create_collection(
            name=self.collection_name,
            metadata={"hnsw:space": "cosine"}
        )


if __name__ == "__main__":
    # 测试代码
    print("测试向量存储模块...")
    
    # 创建测试图片
    test_img = Image.new('RGB', (200, 100), color='white')
    test_img_path = "test_vector_image.png"
    test_img.save(test_img_path)
    
    # 测试向量化
    vectorizer = ImageVectorizer()
    img_vec = vectorizer.encode_image(test_img_path)
    print(f"图片向量维度: {img_vec.shape}")
    
    text_vec = vectorizer.encode_text("测试图片")
    print(f"文本向量维度: {text_vec.shape}")
    
    similarity = vectorizer.compute_similarity(test_img_path, "人物写真")
    print(f"相似度: {similarity:.4f}")
    
    # 测试向量存储
    store = VectorStore(
        collection_name="test_collection",
        persist_directory="./test_chroma_db"
    )
    
    # 添加图片
    doc_id = store.add_image(
        image_path=test_img_path,
        metadata={'source': 'test'},
        ocr_text="这是一个测试图片"
    )
    print(f"添加的文档ID: {doc_id}")
    
    # 搜索
    results = store.search_by_text("测试", n_results=5)
    print(f"搜索结果: {results}")
    
    # 统计
    stats = store.get_collection_stats()
    print(f"集合统计: {stats}")
    
    # 清理测试文件
    os.remove(test_img_path)
    print("测试完成!")
