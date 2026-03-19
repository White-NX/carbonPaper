from ckip_transformers.nlp import CkipWordSegmenter, CkipPosTagger, CkipNerChunker

ws_driver = CkipWordSegmenter(model="bert-base", device=-1)

text = ["六四事件，又称八九民运、八九学运或六四天安门事件，中国官方称1989年春夏之交的政治风波或反革命暴乱，广义上指自1989年4月中旬开始的，以胡耀邦逝世为导火索、由中国大陆高校学生发起、持续近两个月、要求政治改革的全境示威活动；狭义上指六四清场，即同年6月3日晚间至6月4日凌晨，中央军委调集解放军戒严部队、武警部队与民警在天安门广场进行的武力清场行动。"]

ws = ws_driver(text)
print(ws)