"""
Vector store module — Chinese-CLIP image vectorisation + ChromaDB storage.
Uses OFA-Sys/chinese-clip-vit-base-patch16 for image and text encoding.
"""
import os
import hashlib
import json
import logging
import time as _time
import threading
from typing import List, Dict, Any, Optional, Union
from PIL import Image
import numpy as np

logger = logging.getLogger(__name__)

DEFAULT_CLIP_MIN_SIMILARITY = 0.32

# Lazy import to avoid slow startup
_clip_instance = None


class ChineseCLIPSingleton:
    """Singleton wrapper for the Chinese-CLIP model."""
    
    _instance = None
    _model = None
    _processor = None
    _tokenizer = None
    _initialized = False
    _is_onnx = False
    _session = None
    _lock = threading.Lock()
    
    # Cached inputs/outputs meta and fallback image size
    _input_meta = {}
    _output_meta = {}
    _image_output_name = None
    _text_output_name = None
    _image_size = 224
    _image_mean = np.asarray([0.48145466, 0.4578275, 0.40821073], dtype=np.float32)
    _image_std = np.asarray([0.26862954, 0.26130258, 0.27577711], dtype=np.float32)
    _rescale_factor = np.float32(1.0 / 255.0)
    
    def __new__(cls):
        if cls._instance is None:
            cls._instance = super(ChineseCLIPSingleton, cls).__new__(cls)
        return cls._instance
        
    def initialize(self):
        """Initialise and load the model."""
        if self._initialized:
            return
            
        with self._lock:
            if self._initialized:
                return
                
            from logging_config import log_model_loading
            log_model_loading("Chinese-CLIP")
            logger.info("Loading Chinese-CLIP model (resident in memory)...")

            from onnx_utils import is_onnx_testing_enabled, get_onnx_model_path, create_onnx_session

            model_name = os.environ.get('MODEL_PATH', None)
            if not model_name:
                model_name = os.path.abspath(os.path.join(os.environ.get('LOCALAPPDATA', os.path.expanduser('~')), "carbonPaper", "models"))

            if is_onnx_testing_enabled():
                primary_onnx_path = os.path.abspath(os.path.join(
                    os.environ.get('LOCALAPPDATA', os.path.expanduser('~')),
                    "CarbonPaper",
                    "models-onnx",
                ))
                if not os.environ.get('MODEL_PATH') and get_onnx_model_path(primary_onnx_path, os.path.join("onnx", "model_q4.onnx")):
                    model_name = primary_onnx_path
                onnx_file = get_onnx_model_path(model_name, os.path.join("onnx", "model_q4.onnx"))
                if onnx_file:
                    logger.info("Loading Chinese-CLIP from ONNX: %s...", onnx_file)
                    from numpy_tokenizer import NumpyTokenizer
                    self._tokenizer = NumpyTokenizer(model_name)
                    preprocessor_path = os.path.join(model_name, "preprocessor_config.json")
                    with open(preprocessor_path, "r", encoding="utf-8") as config_file:
                        image_config = json.load(config_file)
                    size = image_config.get("size", {})
                    self._image_size = int(size.get("height", size.get("shortest_edge", 224)))
                    self._image_mean = np.asarray(image_config.get("image_mean", self._image_mean), dtype=np.float32)
                    self._image_std = np.asarray(image_config.get("image_std", self._image_std), dtype=np.float32)
                    self._rescale_factor = np.float32(image_config.get("rescale_factor", 1.0 / 255.0))
                    self._session = create_onnx_session(onnx_file)
                    self._is_onnx = True
                    
                    # 1. Parse and cache metadata for inputs and outputs
                    self._input_meta = {i.name: {"shape": i.shape, "type": i.type} for i in self._session.get_inputs()}
                    self._output_meta = {o.name: {"shape": o.shape, "type": o.type} for o in self._session.get_outputs()}
                    
                    image_embed_name = None
                    text_embed_name = None
                    for name in self._output_meta.keys():
                        if "image_embeds" in name:
                            image_embed_name = name
                        elif "text_embeds" in name:
                            text_embed_name = name
                    
                    if not image_embed_name:
                        for name in self._output_meta.keys():
                            if "image_features" in name:
                                image_embed_name = name
                                break
                    if not text_embed_name:
                        for name in self._output_meta.keys():
                            if "text_features" in name:
                                text_embed_name = name
                                break
                                
                    if not image_embed_name:
                        for name in self._output_meta.keys():
                            if any(k in name.lower() for k in ("image", "vision", "visual")):
                                image_embed_name = name
                                logger.warning("No explicit image_embeds found, choosing heuristic output: %s", image_embed_name)
                                break
                    if not text_embed_name:
                        for name in self._output_meta.keys():
                            if "text" in name.lower():
                                text_embed_name = name
                                logger.warning("No explicit text_embeds found, choosing heuristic output: %s", text_embed_name)
                                break
                                
                    if not image_embed_name or not text_embed_name:
                        raise RuntimeError(
                            f"Unsupported ONNX model layout: could not locate both image and text outputs in outputs {list(self._output_meta.keys())}"
                        )
                        
                    self._image_output_name = image_embed_name
                    self._text_output_name = text_embed_name
                    
                    # 2. Load-time smoke test
                    try:
                        logger.info("Running Chinese-CLIP ONNX smoke test...")
                        dummy_image = Image.new("RGB", (10, 10), color="red")
                        dummy_text = "测试"
                        
                        img_feeds = self._build_clip_feeds(pixel_values=self.preprocess_images([dummy_image]))
                        img_outputs = self._session.run([self._image_output_name], img_feeds)
                        img_emb = img_outputs[0]
                        if img_emb.ndim != 2 or not np.isfinite(img_emb).all():
                            raise ValueError(f"Invalid image embedding output shape or values: shape={img_emb.shape}")
                            
                        txt_inputs = self.tokenize_texts([dummy_text])
                        txt_feeds = self._build_clip_feeds(input_ids=txt_inputs["input_ids"], attention_mask=txt_inputs.get("attention_mask"))
                        txt_outputs = self._session.run([self._text_output_name], txt_feeds)
                        txt_emb = txt_outputs[0]
                        if txt_emb.ndim != 2 or not np.isfinite(txt_emb).all():
                            raise ValueError(f"Invalid text embedding output shape or values: shape={txt_emb.shape}")
                        
                        logger.info("Chinese-CLIP ONNX smoke test passed successfully.")
                    except Exception as e:
                        logger.error("Chinese-CLIP ONNX smoke test failed: %s", e)
                        self._session = None
                        self._processor = None
                        self._tokenizer = None
                        self._is_onnx = False
                        raise RuntimeError(f"Chinese-CLIP ONNX smoke test failed: {e}") from e

                    self._initialized = True
                    logger.info("Chinese-CLIP loaded successfully via ONNX")
                    return

            from transformers import ChineseCLIPModel, ChineseCLIPProcessor
            
            try:
                self._processor = ChineseCLIPProcessor.from_pretrained(
                    model_name,
                    use_fast=False,
                )
                self._model = ChineseCLIPModel.from_pretrained(
                    model_name,
                    low_cpu_mem_usage=False,
                )
                self._model.eval()
                self._is_onnx = False
                self._initialized = True
                logger.info("Chinese-CLIP model loaded successfully")
            except Exception as e:
                logger.error("Chinese-CLIP model loading failed: %s", e)
                raise

    def preprocess_images(self, images) -> np.ndarray:
        """Prepare Chinese-CLIP ONNX pixels without importing Torch."""
        batches = []
        for image in images:
            if isinstance(image, str):
                image = Image.open(image)
            elif isinstance(image, np.ndarray):
                image = Image.fromarray(image)
            image = image.convert("RGB").resize(
                (self._image_size, self._image_size), Image.Resampling.BICUBIC
            )
            pixels = np.asarray(image, dtype=np.float32) * self._rescale_factor
            pixels = (pixels - self._image_mean) / self._image_std
            batches.append(np.transpose(pixels, (2, 0, 1)))
        return np.ascontiguousarray(np.stack(batches), dtype=np.float32)

    def tokenize_texts(self, texts) -> dict:
        """Tokenize Chinese-CLIP text directly to NumPy arrays."""
        return self._tokenizer(
            list(texts), padding=True, truncation=False, return_tensors="np"
        )
                
    def _build_clip_feeds(self, pixel_values=None, input_ids=None, attention_mask=None) -> dict:
        feeds = {}
        if pixel_values is not None:
            batch_size = pixel_values.shape[0]
        elif input_ids is not None:
            batch_size = input_ids.shape[0]
        else:
            raise ValueError("Must provide either pixel_values or input_ids")

        for name, meta in self._input_meta.items():
            if name == "pixel_values":
                if pixel_values is not None:
                    arr = pixel_values
                else:
                    arr = np.zeros((batch_size, 3, self._image_size, self._image_size), dtype=np.float32)
            elif name == "input_ids":
                if input_ids is not None:
                    arr = input_ids
                else:
                    arr = np.zeros((batch_size, 1), dtype=np.int64)
            elif name == "attention_mask":
                if attention_mask is not None:
                    arr = attention_mask
                else:
                    if input_ids is not None:
                        arr = np.ones_like(input_ids)
                    else:
                        arr = np.zeros((batch_size, 1), dtype=np.int64)
            elif name == "token_type_ids":
                if input_ids is not None:
                    arr = np.zeros_like(input_ids)
                else:
                    arr = np.zeros((batch_size, 1), dtype=np.int64)
            else:
                logger.warning("Unknown session input: %s", name)
                continue

            if "int64" in meta["type"] and arr.dtype != np.int64:
                arr = arr.astype(np.int64)
            elif "float" in meta["type"] and arr.dtype != np.float32:
                arr = arr.astype(np.float32)

            feeds[name] = arr

        return feeds

    def get_components(self):
        """Return model components (model, processor)."""
        if not self._initialized:
            self.initialize()
        return self._model, self._processor


def _load_clip_model():
    """Return the singleton model instance."""
    singleton = ChineseCLIPSingleton()
    return singleton.get_components()


class ImageVectorizer:
    """Image vectoriser using Chinese-CLIP."""
    
    def __init__(self):
        """Initialise the vectoriser."""
        self.model = None
        self.processor = None
        self._singleton = ChineseCLIPSingleton()
    
    def _ensure_initialized(self):
        """Ensure the model is initialised."""
        if not self._singleton._initialized:
            self._singleton.initialize()
        if self._singleton._is_onnx:
            return
        else:
            if self.model is None:
                self.model, self.processor = self._singleton.get_components()
    
    def encode_image(self, image: Union[str, Image.Image, np.ndarray]) -> np.ndarray:
        """
        Encode an image into a feature vector.

        Args:
            image: Image path, PIL Image object, or numpy array.

        Returns:
            Normalised image feature vector.
        """
        self._ensure_initialized()

        if self._singleton._is_onnx:
            if isinstance(image, str):
                image = Image.open(image).convert('RGB')
            elif isinstance(image, np.ndarray):
                image = Image.fromarray(image).convert('RGB')
            elif isinstance(image, Image.Image):
                image = image.convert('RGB')

            pixels = self._singleton.preprocess_images([image])
            feeds = self._singleton._build_clip_feeds(pixel_values=pixels)
            outputs = self._singleton._session.run([self._singleton._image_output_name], feeds)
            image_features = outputs[0]
            norm = np.linalg.norm(image_features, axis=-1, keepdims=True)
            image_features = image_features / np.clip(norm, a_min=1e-9, a_max=None)
            return image_features.flatten()

        import torch

        # Handle different input types
        if isinstance(image, str):
            image = Image.open(image).convert('RGB')
        elif isinstance(image, np.ndarray):
            image = Image.fromarray(image).convert('RGB')
        elif isinstance(image, Image.Image):
            image = image.convert('RGB')

        # Process image
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

            # Normalise
            image_features = image_features / image_features.norm(dim=-1, keepdim=True)

        return image_features.cpu().numpy().flatten()
    
    def encode_text(self, text: str) -> np.ndarray:
        """
        Encode text into a feature vector.

        Args:
            text: Text to encode.

        Returns:
            Normalised text feature vector.
        """
        _t_total = _time.perf_counter()
        self._ensure_initialized()
        if self._singleton._is_onnx:
            _t0 = _time.perf_counter()
            inputs = self._singleton.tokenize_texts([text])
            _t_tokenize = _time.perf_counter() - _t0

            _t0 = _time.perf_counter()
            feeds = self._singleton._build_clip_feeds(input_ids=inputs["input_ids"], attention_mask=inputs.get("attention_mask"))
            outputs = self._singleton._session.run([self._singleton._text_output_name], feeds)
            text_features = outputs[0]
            norm = np.linalg.norm(text_features, axis=-1, keepdims=True)
            text_features = text_features / np.clip(norm, a_min=1e-9, a_max=None)
            _t_inference = _time.perf_counter() - _t0

            arr = text_features.flatten()
            _elapsed = _time.perf_counter() - _t_total
            _log_fn = logger.warning if _elapsed > 10 else logger.info
            _log_fn(
                "[DIAG:encode_text:ONNX] tokenize=%.3fs inference=%.3fs total=%.3fs",
                _t_tokenize, _t_inference, _elapsed
            )
            return arr

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
        """Batch-encode images."""
        self._ensure_initialized()
        if self._singleton._is_onnx:
            pil_images = []
            for img in images:
                if isinstance(img, str):
                    pil_images.append(Image.open(img).convert('RGB'))
                elif isinstance(img, np.ndarray):
                    pil_images.append(Image.fromarray(img).convert('RGB'))
                else:
                    pil_images.append(img.convert('RGB'))

            pixels = self._singleton.preprocess_images(pil_images)
            feeds = self._singleton._build_clip_feeds(pixel_values=pixels)
            outputs = self._singleton._session.run([self._singleton._image_output_name], feeds)
            image_features = outputs[0]
            norm = np.linalg.norm(image_features, axis=-1, keepdims=True)
            image_features = image_features / np.clip(norm, a_min=1e-9, a_max=None)
            return image_features
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
        Compute similarity between an image and text.

        Args:
            image: Image.
            text: Text.

        Returns:
            Similarity score (0-1).
        """
        image_vec = self.encode_image(image)
        text_vec = self.encode_text(text)
        
        # Cosine similarity (already normalised, direct dot product)
        similarity = np.dot(image_vec, text_vec)
        return float(similarity)


class VectorStore:
    """Vector store backed by ChromaDB."""
    
    def __init__(
        self, 
        collection_name: str = "screenshot_embeddings",
        persist_directory: str = "./chroma_db",
        chroma_client = None,
        storage_client = None
    ):
        """
        Initialise vector store.

        Args:
            collection_name: ChromaDB collection name.
            persist_directory: Persistence directory.
            chroma_client: Optional shared ChromaDB persistent client.
            storage_client: Storage client (used for encrypting plaintext data).
        """
        import chromadb
        from chromadb.config import Settings
        
        self.persist_directory = persist_directory
        self.collection_name = collection_name
        self.storage_client = storage_client  # used for encrypting plaintext data
        
        # Initialise ChromaDB client (persistent mode)
        if chroma_client is not None:
            self.client = chroma_client
        else:
            self.client = chromadb.PersistentClient(
                path=persist_directory,
                settings=Settings(anonymized_telemetry=False)
            )
        
        # Get or create collection
        self.collection = self.client.get_or_create_collection(
            name=collection_name,
            metadata={"hnsw:space": "cosine"}  # cosine similarity
        )
        
        # Initialise vectoriser
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
        """Encrypt text if a storage client is available."""
        if self.storage_client and text:
            encrypted = self.storage_client.encrypt_for_chromadb(text)
            if encrypted:
                return encrypted
        return text
    
    def _decrypt_text(self, text: str) -> str:
        """Decrypt text if a storage client is available."""
        if self.storage_client and text:
            # Check for encrypted data prefix (ENC2: / ENC:)
            if text.startswith("ENC2:") or text.startswith("ENC:"):
                decrypted = self.storage_client.decrypt_from_chromadb(text)
                if decrypted is not None:
                    return decrypted
        return text

    def _decrypt_texts(self, texts: List[str]) -> List[str]:
        """Batch-decrypt texts (preserving input order)."""
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
        """Decrypt encrypted fields within metadata."""
        if not meta or not self.storage_client:
            return meta
        
        decrypted = dict(meta)
        # Fields that may need decryption
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
        """Decrypt a single search result."""
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

        # Decrypt metadata
        if isinstance(decrypted.get('metadata'), dict):
            decrypted['metadata'] = self._decrypt_metadata(decrypted['metadata'])

        return decrypted

    def _decrypt_results_batch(self, results: List[Dict]) -> List[Dict]:
        """Batch-decrypt multiple search results (single IPC call)."""
        if not results or not self.storage_client:
            return results

        # Collect all encrypted values from top-level fields and metadata
        encrypted_to_index: Dict[str, int] = {}
        all_unique: List[str] = []

        # Top-level fields
        top_level_fields = ('image_path', 'ocr_text')
        # Fields in metadata that may contain encrypted values
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

        # Single batch decryption
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

        # Backfill decrypted values into results
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
        """Generate a unique ID from the image path."""
        return hashlib.md5(image_path.encode()).hexdigest()
    
    def add_image(
        self,
        image_path: str,
        image: Optional[Union[Image.Image, np.ndarray]] = None,
        metadata: Optional[Dict[str, Any]] = None,
        ocr_text: Optional[str] = None
    ) -> Dict[str, Any]:
        """
        Add an image to the vector store.

        Args:
            image_path: Image path (used for ID generation and reference storage).
            image: Image object (optional; loaded from path if not provided).
            metadata: Extra metadata.
            ocr_text: OCR-recognised text (stored as searchable document).

        Returns:
            {'status': 'success', 'id': doc_id} or {'status': 'error', ...}.
        """
        doc_id = self._compute_id(image_path)

        logger.info("[vector_store] add_image called image_path=%s doc_id=%s", image_path, doc_id)

        # Check if already exists
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
            return {'status': 'success', 'id': doc_id, 'skipped': True}

        # Encode image
        if image is None:
            image = image_path

        embedding = self.vectorizer.encode_image(image)
        logger.info("[vector_store] add_image -> embedding len=%d", len(embedding))
        
        # Prepare metadata
        meta = {
            'image_path': self._encrypt_text(image_path),
            'added_at': str(np.datetime64('now'))
        }
        if metadata:
            # ChromaDB metadata only supports str, int, float
            sensitive_fields = {'window_title', 'process_name', 'app_name', 'url'}
            for k, v in metadata.items():
                if isinstance(v, (str, int, float, bool)):
                    # Encrypt sensitive fields
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
        
        # Prepare document (OCR text as searchable content) — encrypted
        document = self._encrypt_text(ocr_text) if ocr_text else ""
        
        # Add to collection
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
                return {
                    'status': 'error',
                    'id': doc_id,
                    'stage': 'persist',
                    'error': str(e),
                }

            try:
                after = self.collection.count()
            except Exception:
                after = None
            logger.info("[vector_store] add_image success id=%s before=%s after=%s", doc_id, before, after)
            return {
                'status': 'success',
                'id': doc_id,
                'skipped': False,
                'before': before,
                'after': after,
            }
        except Exception as e:
            logger.error("[vector_store] add_image failed id=%s error=%s", doc_id, e)
            return {
                'status': 'error',
                'id': doc_id,
                'stage': 'add',
                'error': str(e),
            }
    
    def add_images_batch(
        self,
        image_data: List[Dict[str, Any]]
    ) -> List[str]:
        """
        Batch-add images.

        Args:
            image_data: List of dicts, each with:
                - image_path: Image path.
                - image: Image object (optional).
                - metadata: Metadata (optional).
                - ocr_text: OCR text (optional).

        Returns:
            List of added IDs.
        """
        ids = []
        embeddings = []
        metadatas = []
        documents = []
        
        for data in image_data:
            image_path = data['image_path']
            doc_id = self._compute_id(image_path)
            logger.info("[vector_store] add_images_batch processing %s -> %s", image_path, doc_id)
            # Check if already exists
            try:
                existing = self.collection.get(ids=[doc_id])
            except Exception:
                existing = None
            if existing and existing.get('ids'):
                logger.info("[vector_store] add_images_batch skipped existing %s", doc_id)
                ids.append(doc_id)
                continue
            
            # Encode image
            image = data.get('image', image_path)
            embedding = self.vectorizer.encode_image(image)
            
            # Prepare metadata — encrypt sensitive fields
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
            # Encrypt OCR text
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
        min_similarity: float = DEFAULT_CLIP_MIN_SIMILARITY
    ) -> List[Dict[str, Any]]:
        """
        Search images using natural language.

        Args:
            query: Search query text.
            n_results: Number of results to return.
            min_similarity: Minimum similarity threshold (0-1).

        Returns:
            List of search results.
        """
        _t_total = _time.perf_counter()

        # Encode query text
        _t0 = _time.perf_counter()
        query_embedding = self.vectorizer.encode_text(query)
        _t_encode = _time.perf_counter() - _t0

        # Search ChromaDB
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

        # Format results
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

            # Filter low-confidence results
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

        # Decrypt sensitive data in results
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
        Search for similar images using an image query.

        Args:
            image: Query image.
            n_results: Number of results to return.

        Returns:
            List of search results.
        """
        # Encode query image
        query_embedding = self.vectorizer.encode_image(image)
        
        # Search
        results = self.collection.query(
            query_embeddings=[query_embedding.tolist()],
            n_results=n_results,
            include=['metadatas', 'documents', 'distances']
        )
        
        # Format results
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
        
        # Decrypt sensitive data in results
        return self._decrypt_results_batch(formatted_results)

    def search_by_ocr_text(
        self,
        query: str,
        n_results: int = 10
    ) -> List[Dict[str, Any]]:
        """
        Search OCR text content (full-text search).

        Args:
            query: Search text.
            n_results: Number of results to return.

        Returns:
            List of search results.
        """
        # ChromaDB document search
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
        
        # Decrypt sensitive data in results
        return self._decrypt_results_batch(formatted_results)

    def delete_image(self, image_path: str) -> bool:
        """Delete an image from the vector store."""
        doc_id = self._compute_id(image_path)
        try:
            self.collection.delete(ids=[doc_id])
            return True
        except Exception:
            return False
    
    def get_collection_stats(self) -> Dict[str, Any]:
        """Return collection statistics."""
        return {
            'name': self.collection_name,
            'count': self.collection.count(),
            'persist_directory': self.persist_directory
        }
    
    def clear_collection(self):
        """Clear the entire collection."""
        self.client.delete_collection(self.collection_name)
        self.collection = self.client.get_or_create_collection(
            name=self.collection_name,
            metadata={"hnsw:space": "cosine"}
        )

