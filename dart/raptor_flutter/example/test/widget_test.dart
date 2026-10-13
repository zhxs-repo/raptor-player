// Smoke test for the example shell only.
//
// Anything that touches the native library (RaptorBindings.load, a live
// RaptorClient) cannot run in a host test -- there is no libraptor_ffi.so for
// the test runner to dlopen. The real verification is `flutter run` on a device;
// see docs/Android构建.md.

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:raptor_example/main.dart';

void main() {
  testWidgets('renders the player shell', (WidgetTester tester) async {
    await tester.pumpWidget(const RaptorExampleApp());
    await tester.pump();

    expect(find.text('Raptor Player'), findsOneWidget);
    expect(find.byType(CircularProgressIndicator), findsOneWidget);
  });
}
