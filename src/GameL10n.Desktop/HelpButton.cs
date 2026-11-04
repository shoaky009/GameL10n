using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace GameL10n.Desktop;

/// <summary>Contextual help available by pointer, keyboard or touch.</summary>
public sealed class HelpButton : Button
{
    public static readonly DependencyProperty TextProperty = DependencyProperty.Register(
        nameof(Text), typeof(string), typeof(HelpButton), new PropertyMetadata("", OnTextChanged));

    public string Text
    {
        get => (string)GetValue(TextProperty);
        set => SetValue(TextProperty, value);
    }

    public HelpButton()
    {
        Style = (Style)Application.Current.Resources["HelpButtonStyle"];
        Content = new SymbolIcon(Symbol.Help);
        AutomationProperties.SetName(this, "帮助");
    }

    private static void OnTextChanged(DependencyObject sender, DependencyPropertyChangedEventArgs args)
    {
        var button = (HelpButton)sender;
        var text = (string)args.NewValue;
        AutomationProperties.SetHelpText(button, text);
        ToolTipService.SetToolTip(button, new ToolTip
        {
            MaxWidth = 420,
            Content = new TextBlock { Text = text, TextWrapping = TextWrapping.Wrap, MaxWidth = 380 }
        });
        button.Flyout = new Flyout
        {
            Content = new TextBlock { Text = text, TextWrapping = TextWrapping.Wrap, MaxWidth = 380, IsTextSelectionEnabled = true }
        };
    }
}
