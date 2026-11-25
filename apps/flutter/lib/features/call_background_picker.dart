part of 'surfaces.dart';

Future<void> _pickCallBackground(
  BuildContext context,
  WaddleBridge bridge,
  String sid,
) async {
  final current = bridge.call['cameraBackground'];
  final name = await showModalBottomSheet<String>(
    context: context,
    showDragHandle: true,
    builder: (sheet) => SafeArea(
      child: ListView(
        shrinkWrap: true,
        children: [
          for (final entry in const {
            'off': 'No background effect',
            'blur': 'Blur background',
            'mountain': 'Mountain',
            'office': 'Office',
            'abstract': 'Abstract',
            'custom': 'Choose an image…',
          }.entries)
            ListTile(
              leading: switch (entry.key) {
                'off' => const Icon(Icons.block),
                'blur' => const Icon(Icons.blur_on),
                'custom' => const Icon(Icons.add_photo_alternate_outlined),
                _ => Image.asset(
                  'assets/backgrounds/${entry.key}.png',
                  width: 48,
                  height: 32,
                  fit: BoxFit.cover,
                ),
              },
              title: Text(entry.value),
              trailing: current == entry.key ? const Icon(Icons.check) : null,
              onTap: () => Navigator.pop(sheet, entry.key),
            ),
        ],
      ),
    ),
  );
  if (name == null || !context.mounted || bridge.call['sid'] != sid) return;
  Uint8List? bytes;
  if (name == 'custom') {
    bytes =
        await _command(context, bridge, 'file.pickCameraBackground')
            as Uint8List?;
    if (bytes == null || !context.mounted || bridge.call['sid'] != sid) return;
  }
  await _command(context, bridge, 'call.background', {
    'expectedSid': sid,
    'name': name,
    'bytes': ?bytes,
  });
}
