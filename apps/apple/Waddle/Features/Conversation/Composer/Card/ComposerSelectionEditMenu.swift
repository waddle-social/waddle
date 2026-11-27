#if os(iOS)
import UIKit
import WaddleKit

extension ComposerTextView.Coordinator {
    func textView(_ view: UITextView, editMenuForTextIn range: NSRange, suggestedActions: [UIMenuElement]) -> UIMenu? {
        guard view.isFirstResponder, view.isEditable, view.markedTextRange == nil,
              parent.selectionFormats.suppression.isEmpty,
              let selection = ComposerUTF16Selection.scalarRange(of: range, in: view.text),
              let formatting = ComposerSelectionFormatting.inDraft(
                view.text, selection: selection, roomLinks: parent.selectionFormats.roomLinks
              )
        else { return nil }

        let text = view.text
        let actions = ComposerSelectionFormatting.orderedActions.map { action in
            UIAction(
                title: action.title, image: UIImage(systemName: action.symbol),
                state: action.isActive(in: formatting.activeFormats) ? .on : .off
            ) { [weak self, weak view] _ in
                guard let self, let view, view.isFirstResponder, view.markedTextRange == nil,
                      parent.selectionFormats.suppression.isEmpty, view.text == text, parent.text == text,
                      view.selectedRange == range
                else { return }
                report(view)
                parent.selectionFormats.perform(action)
            }
        }
        let format = UIMenu(title: "Format", image: UIImage(systemName: "textformat"), children: actions)
        return UIMenu(children: suggestedActions + [format])
    }

    @available(iOS 26.0, *)
    func textView(_ view: UITextView, editMenuForTextIn ranges: [NSValue], suggestedActions: [UIMenuElement]) -> UIMenu? {
        // One edit in the draft takes one contiguous range.
        guard ranges.count == 1, let range = ranges.first?.rangeValue else { return nil }
        return textView(view, editMenuForTextIn: range, suggestedActions: suggestedActions)
    }
}
#endif
