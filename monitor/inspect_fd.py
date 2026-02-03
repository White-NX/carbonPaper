import fastdeploy as fd
print("Attributes in fd.vision:")
for attr in dir(fd.vision):
    print(attr)

if hasattr(fd.vision, 'vis_ppocr'):
    print("\nfd.vision.vis_ppocr exists")
else:
    print("\nfd.vision.vis_ppocr does not exist")
