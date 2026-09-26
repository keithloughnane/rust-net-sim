#nullable enable
using System;

namespace Emergence.Host
{
    // The host layer's packet streams are plain System.IObservable, so any Rx library (UniRx,
    // System.Reactive, R3's adapters) works on them, but the package depends on none.

    /// <summary>A thread-safe stream that passes each value to everyone watching it.</summary>
    internal sealed class Broadcaster<T> : IObservable<T>, IDisposable
    {
        private readonly object _gate = new object();
        private IObserver<T>[] _observers = Array.Empty<IObserver<T>>();

        public bool IsDisposed { get; private set; }

        public void OnNext(T value)
        {
            IObserver<T>[] observers;
            lock (_gate)
            {
                if (IsDisposed) return;
                observers = _observers;
            }
            foreach (var observer in observers) observer.OnNext(value);
        }

        public IDisposable Subscribe(IObserver<T> observer)
        {
            if (observer == null) throw new ArgumentNullException(nameof(observer));
            lock (_gate)
            {
                if (IsDisposed) return Unsubscriber.None;
                var grown = new IObserver<T>[_observers.Length + 1];
                _observers.CopyTo(grown, 0);
                grown[_observers.Length] = observer;
                _observers = grown;
            }
            return new Unsubscriber(this, observer);
        }

        private void Remove(IObserver<T> observer)
        {
            lock (_gate)
            {
                var index = Array.IndexOf(_observers, observer);
                if (index < 0) return;
                var shrunk = new IObserver<T>[_observers.Length - 1];
                Array.Copy(_observers, 0, shrunk, 0, index);
                Array.Copy(_observers, index + 1, shrunk, index, _observers.Length - index - 1);
                _observers = shrunk;
            }
        }

        public void Dispose()
        {
            lock (_gate)
            {
                IsDisposed = true;
                _observers = Array.Empty<IObserver<T>>();
            }
        }

        private sealed class Unsubscriber : IDisposable
        {
            public static readonly IDisposable None = new Unsubscriber(null, null);

            private Broadcaster<T>? _owner;
            private readonly IObserver<T>? _observer;

            public Unsubscriber(Broadcaster<T>? owner, IObserver<T>? observer)
            {
                _owner = owner;
                _observer = observer;
            }

            public void Dispose()
            {
                if (_owner != null && _observer != null) _owner.Remove(_observer);
                _owner = null;
            }
        }
    }

    /// <summary>The values of a stream that pass a test.</summary>
    internal sealed class Filtered<T> : IObservable<T>
    {
        private readonly IObservable<T> _source;
        private readonly Func<T, bool> _test;

        public Filtered(IObservable<T> source, Func<T, bool> test)
        {
            _source = source;
            _test = test;
        }

        public IDisposable Subscribe(IObserver<T> observer) =>
            _source.Subscribe(new Watcher<T>(value =>
            {
                if (_test(value)) observer.OnNext(value);
            }, observer.OnError, observer.OnCompleted));
    }

    /// <summary>An observer made of delegates.</summary>
    internal sealed class Watcher<T> : IObserver<T>
    {
        private readonly Action<T> _next;
        private readonly Action<Exception>? _error;
        private readonly Action? _completed;

        public Watcher(Action<T> next, Action<Exception>? error = null, Action? completed = null)
        {
            _next = next;
            _error = error;
            _completed = completed;
        }

        public void OnNext(T value) => _next(value);

        public void OnError(Exception error)
        {
            if (_error == null) throw error;
            _error(error);
        }

        public void OnCompleted() => _completed?.Invoke();
    }
}
